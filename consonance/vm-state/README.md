<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# vm-state

`vm-state` is the versioned, deterministic codec for the non-memory portion of
a VMM snapshot. It contains plain-data records for vCPU state, timers, MSRs,
XSAVE, virtual time, device bytes, and the CPU-contract hash. It has no host or
hypervisor dependencies.

## Format

Version 3 is a little-endian TLV container: a 10-byte header (magic, version,
architecture tag, and section count) followed by the required sections in
ascending tag order. Version 4 appends one required trailing engine-state
section when the engine-owned state is nonempty. X86 version 5 extends the
SREGS and DEBUGREGS fixed records with the captured CPU fields (`flags` and
`pdptrs`) and permits the engine-state section to be absent. Empty engine state
and zero extended x86 fields retain the byte-identical v3 writer shape; an
engine-only state retains the byte-identical v4 shape. ARM continues to use the
v3/v4 record set. Fixed-layout records use zerocopy wire types; variable
sections are length-delimited. MSRs use `BTreeMap` order, and timer entries
retain their firing order, so encoding is independent of insertion order.

`VmState::encode` validates the state before writing. Timer entries must be
strictly ordered by `(deadline, sequence)`, tokens must be unique, and each
sequence must precede `next_seq`. `VmState::decode` is strict and total:
malformed headers, section order, lengths, fields, missing sections,
duplicates, and trailing bytes return typed errors.

`peek_version` validates the magic and reads the version without decoding the
rest of the blob. `VM_STATE_VERSION` identifies the newest writer format;
`VM_STATE_LEGACY_VERSION` names the retained v3 shape. The golden test pins the
bytes of a fully populated legacy-compatible state, and the version tests pin
the v4 reader/writer compatibility path.

## Ownership boundaries

The codec carries `contract_hash` but does not compare it with a live CPU
contract; `vmm-core` performs that check during restore. The device section is
an opaque byte payload owned by the VMM and can evolve as a unit when the
snapshot version changes. Snapshot quiescence and any armed injection state are
also enforced by `vmm-core`, not inferred by this codec.

The crate is pure logic and is used by `vmm-core` for snapshot capture and
restore. Run its checks with:

```sh
cargo test -p vm-state
cargo fmt --all -- --check
```
