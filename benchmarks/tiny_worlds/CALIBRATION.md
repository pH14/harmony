# Local game correspondence

These comparisons use ordinary recorded game inputs, a native QuickNES core,
and the production archive or campaign engine. No game memory was edited.
Commercial assets, snapshots, input tapes, and detailed traces remain private.
The public tiny worlds are original mechanics and need none of those assets.

## Scope of the first three families

| Mechanism | Native comparison | Result and limit |
| --- | --- | --- |
| Resource retention and a five-charge barrier | Metroid natural missile replenishment, same-key arrivals, and a shared continuation through a red door | Keeping missile count retains the stocked arrival; hiding it retains the earlier understocked arrival. The supplied continuation crosses only from the stocked arrival. This is local retention/access evidence, not autonomous route discovery. |
| History retention | SMB 7-4 natural correct/wrong-history arrivals with a third same-slot competitor | Preserving history retains the useful arrival in both orders; clearing it evicts that arrival. Direct replay exits the loop only from the useful arrival. |
| Changing action effectiveness | SMB water-section campaigns with a full button alphabet versus right only | Development success was 3/3 versus 0/3 in 2-2 and 2/3 versus 0/3 in 7-2; held-out 8-4 was 1/3 versus 0/3 at 20,000 emulated frames. This demonstrates continued action exploration, not learned adaptation. |

## History comparison

Two starts along the same route were registered separately as development and
validation. Each used three selector seeds, both arrival orders, a 512-entry
archive, and the production capacity of two states per slot. A third naturally
reached wrong-history state creates actual eviction pressure. Warmup excludes
the compared slot, so the three endpoint insertions determine its contents.

| Start | Useful endpoint retained, normal / broken | Shared-suffix successes among selected snapshots, normal / broken |
| --- | --- | --- |
| Before the upper-route jump | 6/6 / 0/6 | 26/96 / 0/96 |
| On the upper route, held out | 6/6 / 0/6 | 14/96 / 8/96 |

Each 96-snapshot denominator is three seeds times two orders times 16 actual
parent selections. Repeated selections and shared ancestry are not independent
game trials. Populations contained 27 versus 20 active states in development
and remained bounded in validation. The useful endpoint succeeds under direct
replay in both arms; the ablation changes its retention, not its mechanics.

Earlier autonomous 7-4 campaigns were inconclusive: 1/3 versus 2/3 at the first
loop and 0/3 in both arms at the second. Those results remain negative evidence;
the later retention experiment supports the narrower mechanism claim above.

## Resource comparison

The door trace identifies the actual red-door object. Its remaining-hit field
counts down from five to one after four missile hits; the fifth hit changes the
open state, and the recorded route crosses into the adjoining room. A separate
one-frame replenishment trace changes missiles from zero to two while health
and every other archive-key field stay fixed.

The door comparison uses a naturally rejoined pair: two versus six missiles,
the same archive place, progress and identity, and unchanged health. A fixed
613-frame continuation collects two additional missiles on the understocked
route, reaches the door with four, and leaves one hit outstanding. The stocked
route opens the door and crosses. The states differ in position and elapsed
time within the archive bucket; they are not identical emulator states with a
single memory field changed. Only the archive-key ablation is a single-factor
intervention.

The held-out pair rejoins a few pixels earlier on the same route. Three seeds
repeat selector behavior; they do not constitute three independent routes.
Both isolated arrival orders retain and select the stocked state under the
production key and the understocked state with missiles hidden. Populated
archive selection remains a separate endpoint. The active representative of the
compared slot has six missiles and crosses in every normal trial; it has two
missiles and fails in every broken trial. The requested later stocked snapshot
itself is not inserted because an earlier stocked representative already occupies
that slot. Randomly sampled population suffix successes are 1/48 versus 1/48 in
development and 2/48 versus 2/48 in validation; no population success-frequency
improvement is claimed. The final two panels completed in 24.79 seconds total,
with one CPU slot and approximately 40 MB sampled peak process-group RSS.

## Action and transition limits

Water campaigns use one worker and one admission reservation. Each verifies
campaign replay, the objective witness, and independently summed execution work.
The full alphabet and right-only control use identical hold lengths and the
existing biased-half mixture. The water fixture does not introduce an online
policy that detects a change of regime.

An ordinary 8-4 tape also crosses land, water, then land. Fixed short input
comparisons corroborate the local change in useful controls. Geometry differs
between these sections, so this is contextual evidence rather than an isolated
measurement of water physics. Obstructed and fatal attempted land baselines
were retained before locating a traversable baseline.

## Reproducibility and interpretation

Private registrations pin ROM, core, input, binary and panel hashes, objective
coordinates, seeds, limits and controls before validation. Native core identity
is QuickNES commit `26bb785c9deddb66a17717b21bb4e328f03ade32`; no bit-identical
snapshot claim is made across native and historical Linux builds. Builds and
experiments run through the shared resource supervisor.

The retention tools restore actual selected snapshots and apply a known shared
suffix only after selection. That suffix is evaluator evidence, never a search
oracle. Natural roots share route ancestry and several experiments reuse
nearby positions. These results calibrate local mechanics and retention/access
signatures; they do not establish a general policy ranking or full-game gains.
