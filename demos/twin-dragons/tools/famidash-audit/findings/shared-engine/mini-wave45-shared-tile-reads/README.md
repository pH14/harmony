This audit integration shares collision history and map reads between the
mini-wave45 repair and the existing spike/wave66 checks. The standalone
[mini-wave45 packet](../mini-wave45/README.md) contains the original Fingerdash
reproduction and patches against pristine upstream source.

The flag table dispatches steep slopes for both wave sizes and the four 45°
triangles for mini-wave only. Diagonal crossings still query intermediate
tiles when both endpoints are empty. The triangle kernel consumes those
existing query results; it adds no map reads or separate history arrays.

The release V9 build kills the unchanged Fingerdash input at frame 3,893.
Bank-aware CPU replay confirms the triangle contact on the death frame.
Ten retained negative inputs pass strict approach, death and reset checks in
both QuickNES and FCEUmm, including the Aftercatabath timing regression, both
wave66 sizes, normal-wave45, Xstep and three Kappa entrances.

Fresh Dissonance searches on the combined builds find Fingerdash clears of
5,069 release frames and 5,073 current frames. Full FCEUmm and CPU replays
agree, with zero source mismatches, no mini-wave45 intersections across 326
mini-wave motions per control, and no wave visits to closed components without
sprites. Other source regions still require intent assessment. The previous
release clearing path used the confirmed escape and is rejected by the fix.

The current Hell control clears in 4,389 frames, preserving the previous
control's gameplay transitions with only duplicate observation durations
changed. Mesen verifies the HUGE clear with the same invariant, excluding
physical level identity. Retiming applies only to positive clearing controls.

All four release sets, all five current sets and current HUGE compile. Decoded
geometry matches source for all 109/170/173 levels. Actual assembled query
gates pass 131,072 cases per release/current/HUGE layout, and selected direct
calls pass physical PRG bank checks. Host geometry and original point-mask
checks also pass for both integrated kernels.

Current integration first overfilled the sound bank, then exposed a separate
layout mistake: putting the new segment before `PCM_BANK` displaced sample
data despite preserving its bytes. Current V10 places the routine after that
segment. All six current builds preserve sound and both PCM segments' CPU
addresses, physical ROM offsets and bytes; all four release builds pass the
same check. The earlier current V9 layout is rejected and unqualified.

The patches here apply to the earlier combined release V7 and current V8
experimental sources, not directly to pristine upstream. `manifest.json`
records each changed file's baseline and resulting hash. They add no level
edits. Compressed indexes retain exact uncompressed hashes.

Qualification remains open. An old native current mini-wave66 input differs
before its reported contact: the previous combined V8 first differs at frame
961, and current V10 at frame 2,058. QuickNES and FCEUmm therefore reject that
tape as a strict current negative; it was not retimed, and its HUGE negative
was not attempted. Current native mini-wave45 reproduction also remains open.
These limits are retained alongside the passing evidence. Full source review,
the Theory of Everything stair revision, and three clean complete rounds are
still required. No clean-round credit or upstream readiness is claimed.
