# Steep wave slopes: extend continuous checks to both sizes

Fresh Dissonance v3 searches on the official release reproduce a normal-wave
escape in Hell that the mini-only proposal misses. At frame 936 the actual
death probe is `(2780,764)`, in the air portion of `RU66_TOP`. The next probe
at frame 937 is `(2784,760)`, in noncolliding fill behind the slope. The segment
crosses the solid diagonal one third of the way through the frame. Full
bank-aware CPU review finds no earlier wave fill reads and zero source
mismatches. Other earlier cube fill contacts remain an intent-review limit.

QuickNES and FCEUmm verify that the extended prototype kills at frame 937,
with all 936 preceding observations unchanged, and resets normally after 40
released frames. The original negative input is unchanged. Original rendering,
full collision reads, canonical witness, corrected outcome and exact contact
geometry are retained separately. The earlier combined-build lead was not
converted into this negative proof: its input dies before this section on the
original ROM. New original-ROM searches produced the retained case.

The pristine-source proposals include the mini-wave continuous check and
extend its dispatch to normal wave. They retain the eight-piece polygon kernel
and raw death-probe coordinates. Previous mini state is now cached per player;
size changes invalidate the segment, as do mode changes, death, wrap, level
reset and actual coordinate replacements. Release retains its 240-pixel room
encoding. No level data changes. Existing `mini_wave66` names are retained to
keep the extension small. These patches supersede the standalone mini-only
patches; apply one matching pristine-source proposal, not both.

The actual kernel passes 64,850 independent exact rational clipping cases
and 65,536 signed products. All four release, five current standard and current
HUGE builds compile. Decoded geometry matches 109, 170 and 173 levels, and all
ten builds preserve every byte of the 2305-byte sound-effect segment. These
checks do not qualify every runtime path or frame budget.

Existing mini-wave negatives still die at release frame 922 and current frame
3064 with unchanged approaches in QuickNES and FCEUmm. Mesen HUGE also passes
the current mini-wave negative and reset check, with only intro differences
and physical level identity excluded. This is compatibility coverage, not a
native-current reproduction of the new normal-wave bug. Focused current
searches have not reproduced that case; current normal-wave runtime coverage
remains open.

A fresh patched Dissonance clearing input completes in 4098 frames, with its
entire input and trace exactly matching the original ROM. Full FCEUmm and CPU
review pass: 38 probes in the early steep-slope section, zero entries into
its enclosed fill and zero source mismatches. One later normal-wave fill read
at frame 1063 requires separate 45-degree/source assessment. This qualifies
the steep-slope section only.

The retained 4103-frame genesis clearing input is **rejected as a section
control**. It still enters component 92 through a later 45-degree boundary,
then uses other filled regions. Its unchanged paired replay demonstrates why
completion and unchanged controls alone cannot establish a legitimate route.
The separate normal-wave45 proposal addresses a different collision family;
it is deliberately absent from this isolated experiment.

Two earlier current mini-only clearing inputs remain exactly unchanged after
this extension (4388 and 4377 frames). Full FCEUmm and bank-aware CPU review
pass, with no wave fill reads anywhere in either trace and zero source
mismatches. Mesen HUGE completes the 4388-frame input with only intro frame 6
differing, excluding physical level identity. Other modes and objective routes
still need source intent review.

This is a source experiment with a confirmed release normal-wave reproducer.
Current normal-wave reproduction, broader orientation/route coverage and
combined-build integration remain open. It supplies no clean-round credit.
The source manifests pin revisions, file hashes and tested configurations;
`evidence-sha256.json` binds every retained evidence file.
