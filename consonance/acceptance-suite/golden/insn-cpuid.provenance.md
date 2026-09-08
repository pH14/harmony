<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# `insn-cpuid.digest` provenance

`insn-cpuid.digest` is the SHA-256 observable digest used by the hardware O2
conformance gate. It covers the guest report stream and serial banner. The
digest is captured by the patched-KVM corpus test and is not edited manually.

## Capture identity

| Field | Value |
|---|---|
| Digest | `cd321ad6f98a9b33f1277a243b06ff5eb5390b652b31f74bb8c39a55f87282f5` |
| CPU | Intel Core i9-9900K, Coffee Lake-S, family 6, model `0x9e`, stepping `0xc` |
| Host class | `det-cfl-v1` |
| Microcode | `0xf8` |
| Host kernel | `6.12.90+deb13.1-amd64` |
| Contract | `../../vmm-core/contracts/x86/intel.toml`, version 4 |
| Contract SHA-256 | `c116b4487137c3e3481c45a5944349fa00223a0d815889ffaf07341b0ebac25a` |
| Payload SHA-256 | `e57784e483d3add5c67ee2b06803b0ba96ebc54691179de71df842aecdf471a9` |
| Seed | `0x0028_C0FF_EE5E_EDC0` |
| Guest RAM | 256 MiB |

The original host captures and contract are preserved in Git history before
the hardware-baseline cleanup. These are capture provenance, not host requirements.