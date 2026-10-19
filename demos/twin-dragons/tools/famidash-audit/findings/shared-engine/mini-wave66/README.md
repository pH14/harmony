# Mini-wave 66-degree corner tunneling

Ordinary controller inputs reproduce escapes through two opposite steep-slope
corners in Hell. Release frame 921 samples `RD66_TOP` at local (13,2); frame
922 lands in noncolliding terrain behind it. Current frame 3063 samples
`RU66_TOP` at (12,14); frame 3064 enters the outlined brick triangle. The
segment between samples crosses the solid polygon. The current case does not
involve the source's separate mini-wave LU66 exceptions. Full release CPU
review finds no earlier normal-wave noncolliding-interior entry on this input.

The isolated source proposals add continuous checks for all eight 66-degree
pieces during mini-wave movement. They use the existing death-probe position,
cache previous per-player collision reads, and query the two other touched
tiles only when both axes change tiles. History is invalidated on mode changes,
wrap, death, level resets and actual teleport/player-coordinate replacements.
Release queries preserve the 240-pixel room encoding. No level data changes.
The manifests pin pristine source commits and every changed file. Apply the
matching patch with `git apply --check` followed by `git apply` in that source.

QuickNES and FCEUmm verify both negative cases: death at release frame 922 and
current frame 3064, every preceding gameplay observation unchanged, followed
by normal resets. Mesen HUGE kills the current input at 3064 and resets;
excluding physical level identity, only intro frame 6 differs before death.
The original current CPU window binds to the separately retained clearing-input
container, whose controller input equals the canonical witness.

The actual header's host kernel passes 64,850 exact rational clipping cases
and all 65,536 signed-byte products. Four release, five current standard and
current HUGE ROMs compile; decoded geometry matches 109, 170 and 173 levels.
NSF, album and VS configurations have not been qualified. These checks do not
establish mapper behavior, frame-budget safety or every intended route.

Two fresh Dissonance v3 clearing inputs finish in 4,388 and 4,377 frames.
FCEUmm verifies their complete patched traces. Full bank-aware CPU review
finds 114 and 113 mini-wave centre probes, no source disagreements and no
mini-wave noncolliding-interior samples. Both traverse the late mini-wave
section containing the repaired triangle. Each identical controller input
and complete trace also matches pristine current upstream in QuickNES, with
no timing conversion. These are mode-specific playability controls: other
modes and objectives still need source assessment. A clearing route alone
is not evidence that every section is legitimate.

The pinned release checkout contains stale generated D-side assets. Four
files were aligned to the exact official generated assets before rebuilding
D and verifying its geometry; the retained alignment manifest records their
hashes. They are deliberately excluded from the source patch. The C-side
negative proof is unaffected. Use verified official exports or regenerate
consistent level exports before treating a full release matrix as valid.

The combined candidate additionally needs separate integration checks. Its
other engine repairs can change earlier contacts or introduce repeated frames,
so its replays do not replace these isolated unchanged-approach proofs. Broader
source review remains incomplete and no clean round is credited.

A third original input independently enters an outlined steep-slope block in
Aftercatabath. It samples `RD66_TOP` at local (13,1) on frame 482, then
`LU66_BOT` at (0,8) on frame 483. Here the original mini-wave LU66 exception
also contributes. The same isolated release patch kills at 483 with every
preceding gameplay observation unchanged in QuickNES and FCEUmm, then resets
normally. The actual original rendered view, full independent replay and
bank-aware CPU window are retained. No additional source edit was needed.
