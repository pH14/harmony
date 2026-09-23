<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# vm-state

`vm-state` is the versioned, deterministic codec for the non-memory portion of
a VMM snapshot. It contains plain-data records for vCPU state, timers, MSRs,
XSAVE, virtual time, device bytes, and the CPU-contract hash. It has no host or
hypervisor dependencies.

## Format

Version 6 is a little-endian TLV container: a 10-byte header (magic, version,
architecture tag, and section count) followed by sections in ascending tag
order. X86 records always use the current SREGS and DEBUGREGS layouts with the
captured CPU fields (`flags` and `pdptrs`). The engine-state and
`xsave_restore_bv` sections are optional within this current format. ARM uses
the same current version and retains its complete architecture-specific record
set. Older output versions are rejected rather than decoded or re-emitted.
Fixed-layout records use zerocopy wire types; variable sections are
length-delimited. MSRs use `BTreeMap` order, and timer entries retain their
firing order, so encoding is independent of insertion order.

`VmState::encode` validates the state before writing. Timer entries must be
strictly ordered by `(deadline, sequence)`, tokens must be unique, and each
sequence must precede `next_seq`. `VmState::decode` is strict and total:
malformed headers, section order, lengths, fields, missing sections,
duplicates, and trailing bytes return typed errors.

`SnapshotRecords::encode_for_hash` is the encoding a caller hashes rather than
stores. It defaults to `encode`; the ARM implementation zeroes the virtual
counter, which runs off the host counter and therefore differs between two runs
of the same guest. The stored encoding keeps the counter, because a restore
needs the value the guest was reading.

`peek_version` validates the magic and reads the version without decoding the
rest of the blob. `VM_STATE_VERSION` identifies the only writer and reader
format. The restore-bits section is validated for wire shape here; vmm-core owns
any backend-specific validation of the captured XSAVE image.

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
