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
- `/usr/lib/harmony/supervisor` SHA256 `f5dd3b5dcf3f3fcf19ae1fa2c92eefe981a32ab65dd2195d210425a7b3fa5458`: 0x62ca2 (1 proof instructions)

Published fef80533 delta: only the supervisor file changes. Its exact wrapper
[0x62ca0,0x62cad) is byte-identical to the prior binary: xor ECX,ECX at0x62ca0,
XGETBV at0x62ca2, combine EDX:EAX and return. The only observed direct reference
into that range is call0xe01c to0x62ca0; relocation tables contain no target
inside the wrapper. The matching Rust nightly std_detect source calls
_xgetbv(0), gated on XSAVE/OSXSAVE. Full disassembly remains on ms02 under
/root/harmony-publisher-fef80533-review. Static evidence still assumes normal
ABI entry, not arbitrary interior indirect transfers or code mutation.
