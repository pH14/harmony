The release Fingerdash mini-wave can pass through a solid 45° slope between
collision samples and continue below the corridor. This is reproduced in the
unmodified official ROM, independently of the visualizer.

At frames 3,892–3,893 the actual collision probes move from (10828, 874) to
(10832, 881). Both endpoint tiles have `COL_NONE`. The diagonal intermediate
tile at column 676, row 25 is `COL_SLOPE_RD45`. Exact clipping of the pixel
centres intersects its solid triangle for the positive interval [9/11, 7/8].
The bank-aware CPU trace confirms the map reads and surviving escape. The
ordinary-input witness retains the first 3,950 frames without retiming.

`release-mini-wave45.patch` adds continuous triangle collision for mini-wave
across all four 45° orientations. It preserves the original stationary point
masks, uses separate history for each player, and invalidates that history on
reset and coordinate replacement. It adds no level edits. Normal wave and
other slope angles remain outside this isolated patch's scope.

QuickNES, FCEUmm and Mesen reproduce the original escape. All three kill the
unchanged input at frame 3,893 with the patch, preserve every preceding
gameplay observation after the entry animation, and confirm an actual reset.
The source review finds 48 live samples below the original floor in the
retained window; the patched trace has only one contact on the death frame.

The host checker executes the actual kernel against 47,426 independent exact
geometry cases, checks all 1,024 stationary points against the original source
predicates, and verifies all 65,536 signed products. The motion reviewer
checks actual consecutive probes, including diagonal intermediate tiles.
Both tools are in the parent audit directory.

Fresh Dissonance WASM v3 searches produced a 5,066-frame patched release clear
and a 5,074-frame original current clear. Full FCEUmm and bank-aware CPU
replays pass. The release control has 331 reviewed mini-wave motions and the
current control has 465, with no mini-wave45 intersection, no source mismatch,
and no live wave visit to a closed component without sprites. Other decorative
regions and source intent still require assessment. The current control also
clears on the patch after changing only repeated observation durations; every
gameplay transition matches. Mesen verifies the current HUGE clear with the
same transition invariant, excluding physical level identity.

All four release sets, all five current sets and current HUGE build. Every
decoded level matches source (109, 170 and 173 levels respectively), collision
tables agree, and all ten builds preserve the original 2,305 sound bytes.
The source revisions and evidence hashes are in `manifest.json`. Compressed
index entries also record their exact uncompressed hashes. Restore an index
before using tools that require plain JSON. The patches apply to pristine
source, independently of the combined experimental repairs.

This remains an isolated experiment. A native current mini-wave45 negative
has not been established: the unchanged release input dies before the target
on current upstream, and a separately discovered current clipping candidate
has no mini-wave45 intersection. Neither is counted as current reproduction.
Integration must preserve bank placement and frame timing while sharing map
reads with the other collision checks. Full source assessment and three clean
complete rounds remain open; this packet earns no clean-round credit.
