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
