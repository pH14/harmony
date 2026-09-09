# Audit payload capacity repair

Consolidation review found that the declared action-capacity bound was enforced
only as an input-length bound. Candidate reconstruction materializes its parent
and appends a suffix. A vector of length7000 can grow to length7001 with
capacity14000. The prior v1 and complete v2 samplers then retained that spare
capacity. This invalidates the strict worst-case capacity interpretation of the
declared2.5/5MiB bounds; it does not change the recorded input actions or outcomes.
Actual total RSS was separately measured, and no historical result is rewritten.

The repair stores each accepted input through a boxed slice and back to a
vector, making its reported capacity equal to its length. Candidate, observed
incumbent and complete-slot inputs all use this path. Temporary reconstruction,
allocation overhead and serialization buffers remain outside the retained action
payload bound. A regression fixture deliberately grows a maximum-length input
past the capacity bound, then requires compact capacity and identical serialized
actions; empty genesis is covered too. No unsafe code or search counter changes.

Before publishing the repair, run the relevant default/all-feature NES tests,
strict all-feature Clippy and formatting. Freeze new source and two ARM builds
as survivor-default-002/survivor-motion-002 without touching earlier bundles.
Run only the two affected5000-job Metroid qualification cells: default v1 and
quality-policy complete v2. Preserve the original requests, use sequential
CPUs8–11 and the existing120s/1GiB external bounds. Both search streams and both
complete audit files must be byte-identical to qualification001. Any mismatch
blocks publication. No suffix experiment or failed retention family is reopened.

The d7d83518 repair passed both ARM qualifications. Default Metroid retained
stream9df5aefe… and audit70d6f23e…; complete quality retained stream2c9f8a8a…
and audit03dfff25…. Full identities and build hashes are in
[u01-capacity-build-provenance.json](u01-capacity-build-provenance.json) and
[the cell evidence](u01-capacity-qualification-results.json). The two cells used
1,388,639 admitted frames plus the same full-replay work,87.09s summed elapsed
time, and no new suffix probes. Default NES tests now pass126 cases; all-feature
release NES tests pass140, including the capacity regression. Strict Clippy and
formatting pass. Both native qualification services are inactive/successful.
