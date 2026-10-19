# Blast Processing: closed saw-lined cavity

Fresh Dissonance searches on the official release and pinned current upstream
find ordinary UFO inputs entering the black interior of a closed, outlined
cavity. Actual bank-aware FCEUmm map reads confirm live entries through the
SMALL_SAW boundary: release frames 4921–4932 and current frames 4934–4939.
Source and rendered-original review identify terrain with no internal sprites
or coin objectives within eight tiles. This is a narrow data repair; it does
not establish that every black fill region should be solid.

The release proposal changes 14 fill cells at columns 848–855, rows 17–20,
to existing solid black metatiles. Current upstream already has four solid
cells on the right edge; its proposal preserves those and changes ten cells.
Exact cells, GIDs, pinned revisions and before/after hashes are retained in the
edit specifications and manifests. Existing edge variants change the interior
line artwork. No engine code changes.

| Check | Release | Current |
| --- | --- | --- |
| First changed contact | 4918 | 4931 |
| Fixed death | 4919 | 4932 |
| Entire unchanged approach | 4917 frames | 4930 frames |
| Paired clearing controls | 5942 / 5947 frames | 5949 / 5949 frames |

The first changed contact is a nonfatal ejection before death. These are
contact corrections, not strict death-only comparisons. Original witnesses
and actual corrected traces are retained separately; full FCEUmm replay and
bank-aware collision reads verify both outcomes, with zero corrected entries
into the edited footprint and zero source mismatches. The original negative
inputs were never retimed. Released-input reset checks pass on release FCEUmm
and current HUGE Mesen.

All four clearing controls use exactly the same actions and complete traces
on original and patched standard ROMs. Full independent FCEUmm replay and CPU
review find zero edited-footprint entries, zero source mismatches and 93 UFO
probes in the affected section per control. No live entries into other closed,
sprite-free fill components occur in those traces; source-edge and objective
routes still require intent review. The current first control preserves two
coins. Mesen HUGE reproduces the current corrected outcome, resets normally,
and completes that 5949-frame control; only two intro observations and physical
level identity differ from standard in the positive replay.

Release A (43 levels), current A/B (80 levels) and HUGE (173 levels) compile
and match decoded source geometry. The current build also contains the
separate Dark Paradise floor repair. Recursive export comparison confirms only
the intended level payloads change: Blast grows 26 bytes on release and 19
bytes on current. Apply the source patch matching the manifest revision,
regenerate A (and current HUGE), and build; generated files are not included.

Discovery used frozen Dissonance WASM v3, with new native-current inputs rather
than converted release negatives. Canonical cases bind ROM/core identities;
CPU evidence binds the cases and symbols. `evidence-sha256.json` records the
retained evidence bytes. Other cavities, whole-route intent and broader
integration remain open. This packet supplies no clean-round credit.
