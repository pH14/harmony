# nes exact selector control-flow review proposal

The artifact digests and every XGETBV site are enumerated in nes-selectors.json.
All retained instruction chains explicitly zero ECX, preserve it through the
bounded whitelist, and contain no observed direct entry bypassing zeroing.
The reviewed interpretation is ordinary compiler/runtime feature-detection
control flow: calls target ABI function entries, not interior XGETBV bytes.
No arbitrary interior function pointers, corrupted control transfers, generated
code or runtime code mutation are supported. This indirect-entry exclusion is
a stated trusted-program assumption for the exact workload/platform code,
not a claim that static linear decoding proves arbitrary binary safety.

- `/bin/busybox` SHA256 `dd40538865c749943b671e11ef9f644f86dd6037abe35dcbbf7f7d67853ae3ee`: 0x4a5c99 (7 proof instructions)
- `/opt/harmony/play-agent` SHA256 `4cc1dacf6ef2aed5638e5eb6eceb7130eca532ed351e3a5b692da0a903a6349a`: 0x1847b9 (7 proof instructions)
