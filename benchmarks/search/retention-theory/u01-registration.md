# U01 complete-survivor diagnostic: frozen selection and analysis

This specifies the suffix diagnostic before inspecting the new complete samples.
The prerequisite is all four exact stream checks in `qualify_survivor_audit.py`,
including the default Metroid v1 audit's exact bytes. Use only the quality-policy
5000-job qualification audit from seed3; this is development evidence.

In the original stratum3 sample order, require a complete two-incumbent record,
exactly one unretained endpoint and two locally retained endpoints, and equality
of all six preference/resource fields across the three states. Choose the first
eight eligible records, then remove records from the end until the conservative
physical bound is at most4M frames. Require at least four, otherwise stop. Do not
substitute later records for an expensive early record. Publish the exact sample
indices, source hashes and bound before running suffixes.

Use seed20261215,16 suffixes of24 actions, corrected terminalv3, CPU8, the frozen
`probe-001` executable, a120s watchdog and64MiB output cap. The bound includes
all reconstructed prefixes, both comparisons' suffixes, and every possible
discarded-gain export. Two pair-probe rows per competition repeat the unretained
endpoint against each survivor. Their unretained outcomes must agree exactly
before analysis. The adapter's unused exposure/creation fields are explicitly
zero placeholders, never measurements. Private input tapes remain on msr1.

For each fixed suffix, compute unretained map/capability events outside the
two-survivor union; also report pairwise differences already covered by the
other survivor. Report endpoint survival separately. For an admitted candidate,
compare the old incumbent union with the local survivor proposal to show actual
gained and lost events. A rejected candidate leaves that union unchanged, so
its exclusive opportunities must not be called actual replacement losses.

No fitted scalar utility, population interval, breakthrough inference or longer
search follows from these descriptive measurements. They test whether the new
audit can resolve the complete-survivor question in a bounded real workload.
The callback precedes global eviction; final global survival remains outside
this claim. Finite no-difference observations do not certify equivalence.
