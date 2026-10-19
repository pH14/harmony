# Dark Paradise: closed brick floor

Ordinary Dissonance inputs enter a closed brick floor in release B1 and
current B2 (HUGE slot 33). Bank-aware FCEUmm reads confirm the first live
interior entries at release frame 3997 and current frame 3814. Source and
original-view review identify a closed floor with no internal sprites or
coin objectives within eight tiles.

The proposal changes only 16 noncolliding fill cells: columns 586–591 at
row 21 and 584–593 at row 22. It uses existing solid black edge variants;
interior line artwork changes. No engine code changes. Exact GIDs, source
revisions and before/after hashes are in the edit specifications and manifests.
This is one reviewed floor, not a rule for converting every closed fill region.

QuickNES and FCEUmm agree on fixed deaths at release frame 3993 and current
frame 3811. Every preceding observation is unchanged (3992 / 3810 frames),
and both reset after 40 released frames. Original full replays and canonical
bank-aware collision windows independently verify the source entries. Mesen
HUGE agrees with the current fixed death and gameplay approach, excluding
physical level identity, and also resets normally. Negative inputs were not
retimed.

Fresh Dissonance searches supply two clearing controls per source version:
4116 / 4120 frames on release and 3935 / 4112 on current. All four inputs
and their entire traces are exactly equal on original and patched standard
ROMs. Full FCEUmm replay and full bank-aware CPU review pass, with zero edited
footprint entries, zero source mismatches and 104 normal-wave section probes
per control. Both current controls preserve Coin2. Mesen HUGE completes the
3935-frame control with no observation differences after excluding physical
level identity, including the intro.

The current 3935-frame control has eleven live reads in another closed fill
region (component 27). It establishes passage through this repaired section,
not whole-route correctness. The other three controls have no live reads in
other closed, sprite-free fill components, but source-edge and objective
routes still require intent review. Five additional Dark Paradise regions
remain leads requiring their own collision and intent assessment.

Release B (31 levels), current A/B (80 levels) and HUGE (173 levels) compile
and match decoded source geometry. The current build also contains the
separate Blast Processing repair. Recursive export comparison confirms only
those intended payloads change; Dark Paradise grows eight compressed bytes
in each configuration. Apply the matching source patch, regenerate B (and
current HUGE), and build. Generated allocation and export files are excluded.

Discovery used frozen Dissonance WASM v3. Current negatives were found on
native current ROMs rather than converted release inputs. Canonical cases and
CPU evidence bind ROM, core, input and symbol identities. Evidence file hashes
are retained in `evidence-sha256.json`. These section results do not qualify
whole-level intent, broader integration or a clean round.
