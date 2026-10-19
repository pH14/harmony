# Preserve Electroman's third-coin corridor

Electroman's two-cell component at columns 796..797, row 24 has no sprites
inside and no `COL_NONE` boundary faces. It is nevertheless part of the
mini-ball route to Coin 3, whose source origin is seven tiles beyond its box.
Partial spike boundaries permit the passage. Do not infer that this component
needs solid caps from tile connectivity alone.

Fresh Dissonance v3 search round 68 on the untouched release ROM found the
retained ordinary input. Actual bank-aware CPU probes enter these two cells
at frames 4625..4632 in mini-ball mode. FCEUmm matches the complete 5037-frame
trace and independently records Coin 3 pickup at frame 4672. The input finishes
with coin bits 5, meaning Coins 1 and 3. The source places Coin 3 at column 804,
row 24. The rendered corridor and hash-bound source disposition are retained.

This protects the coin corridor, not every entry into this component. Other
modes and source intent still need review. Earlier sections of this complete
input are unqualified: the combined repair build stops the same mechanical
route at source frame 2853, well before this corridor. The original input is
therefore a section control, not proof that the whole route uses no bugs.

Component schema 4 reports nearby objectives separately from sprite membership.
An objective can lie outside a component reached through partial boundaries;
its absence inside the box is insufficient evidence for a terrain patch.
