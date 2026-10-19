# Ceiling-slope ejection experiment

A Dissonance controller input in Revolution moves the primary ball into an
outlined, filled shallow ceiling during dual mode. Original CPU reads identify
the first entry at frame 2117. The ceiling component spans columns 388..391,
rows 18..19 and contains no sprites or open faces.

Two collision decisions contribute. The ceiling handler discards a valid left
support contact because of its rising-slope priority rule. The floor handler
then accepts an upside-down `RU22_RIGHT` slope under its bottom probe. It sets
`eject_D=4`; the ball subtracts four pixels from Y, changing its relative fixed
position from 14422 to 13398. This pushes it farther into the ceiling. The
retained instruction-site observations resolve addresses through actual PRG
mapping, and the ordinary replay is checked against the original witness.

The prototype rejects opposite-facing slopes before the slope routine changes
state, and lets the ceiling handler consider both support probes. It changes
two functions and no level data. QuickNES matches every observation through
frame 2116; the first correction is at 2117. FCEUmm verifies the actual repaired
trace. The ball survives below the ceiling. In the retained CPU window neither
player enters the target component. This is a contact correction, not a
negative case that should die, so do not use the death-only `--fixed` checker.

An orientation guard alone is insufficient: one earlier experiment still
entered the ceiling because the valid ceiling contact remained discarded.
Rejecting after the slope routine is also insufficient because it has already
changed slope state. Those versions are not proposed patches.

Fresh Dissonance v3 search round 67 executed 300,000 steps across nine roots.
Inputs from the three main roots finish in 6305, 6309 and 6309 frames. FCEUmm
verifies all three complete traces. Full bank-aware CPU review records 6940,
6942 and 6942 centre probes, no source mismatches, and zero target-component
entries by either player. Each control has 84 ball probes in the repaired
section. These are section-qualified controls; other routes and objectives
remain under source assessment.

Current upstream independently reproduces the same ceiling entry at frame
2128 in a fresh Dissonance round 74 input. The original and repaired traces
agree through frame 2127. FCEUmm verifies both complete 2136-frame traces;
bank-aware reads show nine original target entries and zero repaired entries
by either player. Both inputs eventually die at frame 2136, at different
positions. The repair keeps the ball outside the ceiling during this window.

Fresh Dissonance round 75 finds a current clearing control of 6310 frames,
including Coin3. FCEUmm matches every gameplay frame; full CPU review records
6942 centre probes, 84 ball probes in the affected section, zero entries into
the target component and zero source mismatches. This is a section control,
not qualification of every other contact along the route. Mesen replays the
ordinary contact tape and clearing input on HUGE without retiming: every
observation after frame 40 matches the standard repaired trace, excluding
physical level identity.

The patch applies cleanly to the pinned release and current upstream. Both
original functions are byte-identical between those source revisions.
Release A/B/C and all current A/B/C/D/E/HUGE builds compile. Decoded current
geometry matches all 170 standard and 173 HUGE levels. Other slope
orientations, probe-depth selection, broader landing behavior, frame budget
and remaining release configurations still require validation. The patch is
an experiment, not an upstream-ready repair or a clean-round qualification.
