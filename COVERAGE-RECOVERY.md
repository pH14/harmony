# Coverage recovery (GitHub issue #69)

The CI compile break (Step::SdkStop, fixed in #68) had failed the coverage/mutants/nextest
jobs *before they could measure* since #63 — so tasks 73/78/69 merged without their coverage
gates actually running. #68 restored the region floor to 93.27% by pinning materialize.rs,
but the underlying per-file coverage is thin. This branch ratchets it back up.

Targets (per #69):
- dissonance/conductor/src/record.rs   ~67.5%
- dissonance/conductor/src/campaign.rs ~82.7%
- dissonance/conductor/src/lib.rs      ~71.5%
- dissonance/conductor/src/main.rs     ~0%  (cfg(linux) live-drive bin — box-only)

Goal: the region floor should RATCHET UP past 93.27%, not just sit at it.
