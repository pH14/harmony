# e2a9ac58 selector and incoming-edge review (reviewed)

The selector evidence was regenerated from the exact ELF bytes under
`static/e2a9ac58`, not inferred from the old proof. Focused disassembly is in
`static/proof/*xgetbv.txt`; the structured record is
`static/proof/selector-inventory.json`.

* `/bin/busybox` SHA-256 `ed276986a91b1c13f6da78900446dfa4311e841d87d8f770ae3ca10b18767b70`
  has its sole XGETBV(0) at `0x472762` (`0f 01 d0`). The straight-line chain
  starts with `xor %ecx,%ecx` at `0x472747` and has no ECX write before the
  instruction. The exact control-flow context includes the branch into this
  block and the post-read feature tests.
* `/usr/bin/runc` SHA-256 `ce6353a8273004c5f917277846dd521c7185b653ca46cfe538cc16b1be254cc9`
  has its sole XGETBV(0) at `0x4058e5`. Its ABI helper begins at `0x4058e0`
  with `mov $0x0,%ecx`; the sole observed caller is the direct call at
  `0x405729` to `0x4058e0`.
* The new `/usr/lib/harmony/supervisor` SHA-256
  `6cc0b938db15ccff58e8db84e02ffec09b496d4deb752f2c62dd625c5dc21875` has
  its sole XGETBV(0) at `0x6ce92`. The wrapper is exactly
  `[0x6ce90,0x6ce9d)`, bytes
  `31 c9 0f 01 d0 48 c1 e2 20 48 09 d0 c3`; ECX is zeroed at `0x6ce90`.
  `objdump -dr` finds one direct incoming call, `0xe16c -> 0x6ce90`, and no
  relocation targeting the wrapper or interior. The old wrapper was at
  `0x62ca0`; its bytes were identical, but the address and caller changed.

A full disassembly scan of the three OCI ELFs found no XGETBV(1), XSAVES,
XRSTORS, or other XSAVE-family instructions in runc or supervisor. BusyBox's
save instructions remain in the previously reviewed exact resolver regions;
BusyBox and runc are byte-identical to the accepted fef80533 files.

The new selector proof establishes ECX-zero normal ABI chains only. It does
not establish arbitrary interior-entry safety or cover generated/code-mutated
paths, which remain trusted-scope conditions.
