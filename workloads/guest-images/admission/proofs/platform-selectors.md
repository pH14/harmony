# platform exact selector control-flow review proposal

The artifact digests and every XGETBV site are enumerated in platform-selectors.json.
All retained instruction chains explicitly zero ECX, preserve it through the
bounded whitelist, and contain no observed direct entry bypassing zeroing.
The reviewed interpretation is ordinary compiler/runtime feature-detection
control flow: calls target ABI function entries, not interior XGETBV bytes.
No arbitrary interior function pointers, corrupted control transfers, generated
code or runtime code mutation are supported. This indirect-entry exclusion is
a stated trusted-program assumption for the exact workload/platform code,
not a claim that static linear decoding proves arbitrary binary safety.

- `/bin/busybox` SHA256 `ed276986a91b1c13f6da78900446dfa4311e841d87d8f770ae3ca10b18767b70`: 0x472762 (6 proof instructions)
- `/usr/bin/runc` SHA256 `ce6353a8273004c5f917277846dd521c7185b653ca46cfe538cc16b1be254cc9`: 0x4058e5 (1 proof instructions)
- `/usr/lib/harmony/supervisor` SHA256 `e0ef13ac2e0d37cf0779fc55bb3a6b173e5004d4f27ca6fc5b90b4d9f2488cca`: 0x62ca2 (1 proof instructions)
