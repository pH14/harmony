# Complete local-survivor audit — bounded measurement scope

Registered before implementation on September 9, 2026, about07:19UTC.

The pairwise counterexample in the predictive-retention contract identifies a
measurement gap: a discarded state can differ from the observed worst survivor
while another survivor already covers that future. The existing Metroid pair
audit cannot distinguish these cases. This work adds evidence for that distinction;
it proposes no retention heuristic and reopens no failed allocation gate.

The generic observer will expose a lazy, complete view of the existing local
slot: stable entry id, key, optional cached snapshot, reconstructed input, and
whether the local rule proposes keeping the entry. Candidate admission is already
reported. The callback precedes population and memory eviction, so these are
**local-rule survivors**, never a claim about the final global archive. Default
observers will not materialize this view or allocate its input vectors.

An opt-in NES feature will attach complete competitions to the same independently
sampled pair records. It will retain the existing sample indices and limits,
require at most two incumbents and at most8192 actions per input, and report
missing snapshots or oversized competitions explicitly. Private inputs stay on
msr1. The default audit format and search behavior remain unchanged.

Before a ROM diagnostic: exercise rejection, replacement, missing snapshots and
the complete-survivor counterexample with the real generic archive; check strict
Clippy and relevant replay/pressure tests. Rebuild on msr1 with pinned source and
asset hashes. Reproduce the prior5000-job qualification streams, with fixed job
and frame bounds plus120s watchdog per cell and at most four cells. Any stream
divergence blocks the suffix probe. The cells preserve every original header
bound; an external120s watchdog is tighter. Use CPUs8–11 sequentially and allow
1GiB output per cell, including its checkpoint. Default Metroid must also
reproduce the exact legacy v1 audit bytes. Inspect complete sample availability only
after qualification; do not pick samples by future outcomes.

If qualified, register the exact suffix sample and conservative physical bound
before executing it. The intended maximum is eight complete two-incumbent
competitions, sixteen shared suffixes of24 actions, a4M physical-frame cap,
120s helper watchdog,64MiB output cap and one attempt. Use the existing frozen
equal-suffix executable twice per competition and verify repeated discarded
outcomes agree before forming the two-survivor union. Report survival and local
map/capability events separately; no post-hoc scalar utility. No capability gain
or descriptive fraction from this diagnostic authorizes a longer campaign.

Stop implementation by08:16UTC if it is not qualified; preserve the partial
result and use the reserved consolidation period. The original10:16UTC tranche
deadline and untouched validation panel remain unchanged.
