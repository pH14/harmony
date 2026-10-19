# Shared collision history integration

These deltas reduce the mini-wave66 fix's cost when combined with the other
experimental collision repairs. They apply to the frozen combined V1 source
builds named in their manifests, rather than pristine upstream. Use the small
standalone patches in the parent directory for the isolated upstream proposal.

Mini-wave66 reuses the pit/spike history, bounded-motion and mode-change guards.
It tests the existing previous/current classes and queries the two intermediate
tiles only when both tile axes change. Current upstream additionally caches
normal-wave45 aligned classes and short-circuits the identical triangle
predicate. Query scratch fields are restored, and the previous aligned class
updates after the segment uses its old value.

All four release variants and all six current variants compile. Decoded geometry
matches 109 release, 170 current standard and 173 current HUGE levels. The current
SFX payloads match combined V1. The pure polygon checks pass for both headers;
these checks establish geometry, not an NES frame budget.

Release C strictly preserves the approaches to the retained mini66 and two
normal45 failures, then kills at the reviewed contacts. Mesen agrees on the
mini66 case. The current standard and HUGE normal45 case also passes unchanged
approach/death/reset checks. Current combined mini66 still changes a repeated
frame at frame 961, before the old mini66 witness. That input is unsuitable for
a strict frame-3064 integration proof. The isolated current patch's strict
3064 proof remains in the parent directory.

Fresh Dissonance round 72 on the correct Hell profile finds three clearing
controls of 4388/4390/4390 frames. FCEUmm verifies all complete traces. Full
bank-aware reads show zero mini-wave fill visits, zero aligned normal-wave fill
visits, zero entries into the original target component, and zero source
mismatches. Other modes and coin routes along these controls still need source
review. Two controls port back to the earlier V11 build with every mechanical
transition unchanged: only two duplicate source frames are skipped and one
destination duplicate inserted. Both paired 4389-frame controls pass FCEUmm.
Mesen also completes a corresponding HUGE control with the same gameplay
transitions, allowing duplicate durations and physical level identity to differ.

Earlier focused rounds 69–71 accidentally used Slaughterhouse's profile for
current Hell. Their failure to clear is not playability evidence. The retained
round-72 controls use the checked WindyLandscape alias. None of these artifacts
qualify a full clean round or establish an upstream-ready combined patch.
