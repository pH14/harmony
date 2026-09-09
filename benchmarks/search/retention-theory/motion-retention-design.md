# Bounded context representatives after P04

P04's frozen coarse descriptor separates six of eight useful pairs still merged
by the finer position/pose key: pairs 0, 3, 5, 6, 12 and 13. Pair 0 has identical
recorded mechanical state, but differs in horizontal speed sign and has a
distinguishing existing suffix (trial 15). Pair 4 has identical values even in
the seven-byte motion tuple and still has useful disagreements. Motion is
therefore a concrete candidate descriptor, not sufficient Markov state or an
identified sole cause of the observed differences. All 32 endpoints replayed
twice and retained unchanged snapshots during reads, costing 1,587,338 physical
frames. No new suffixes or search campaigns were run in P04.

The next design preserves the existing retention slots and all selection
groups. Within a slot, retain the ordinary quality maximum and the best-quality
state with a different opaque motion context, at most two representatives under
the same byte budget. No facing or velocity sign is intrinsically preferred.
An ordinary top-two-quality policy is the capacity control; it distinguishes
the descriptor choice from merely allowing another representative. Production
retention remains a separate comparator where needed.

For a fixed candidate stream, each context has a quality maximum. The proposed
rule keeps the two highest-quality context maxima. It can update exactly from
the current two winners plus a candidate: if the candidate improves an existing
winning context, replace that context's maximum; if it creates or improves an
excluded context enough to enter the top two, discard the lower winner. Any
excluded context's previous maximum was below both current winners and cannot
become relevant without a newly observed improvement. This induction assumes
stable quality/context and excludes external evictions/imports and exact-input
deduplication, as the earlier streaming claims do.

This is not a reachability guarantee. A third, lower-quality context can contain
the only useful continuation and be discarded. Two states sharing a context
can also have different futures. Finite fixtures must include both a case where
the context distinction preserves a lost motion-dependent exit and a case
where this capacity limit loses an exit. Test the actual archive implementation,
not a separate idealized retention function. Full production coordinator replay
must exercise real alternatives, memory pressure and continuation dispatch.

Implementation boundary: add an optional opaque context method to the generic
archive key contract, plus explicit two-quality and two-context policy IDs.
Metroid can derive the three-byte context from its already cached current WRAM
at candidate creation and snapshot-key reconstruction. A versioned, opt-in
build feature may add the context to its key; it must not change observations,
snapshots, geometry, reward, terminal handling or action vocabulary. Both
experimental arms use the same feature-enabled binary and metadata. The
feature-disabled build must preserve historical behavior and streams. Do not
reinterpret the existing raw pose byte to hide the new context. Missing opaque
contexts follow the ordinary retention rule; they do not become fake contexts.

Before a fresh campaign: implement the bounded rule, its finite positive and
negative examples and replay checks; qualify the context read from cached WRAM
against P04's direct reads; verify versioned identities and default compatibility.
Then specify a small distinguishing pilot and its work limit before running it.
P04's pass authorizes that design and qualification work only. The failed K01
and Metroid R03 campaign gates remain recorded and are not reset. No long run,
default change or held-out panel is yet justified by this proposal.
