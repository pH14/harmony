<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# vm-state

`vm-state` is the versioned, deterministic codec for the non-memory portion of
a VMM snapshot. It contains plain-data records for vCPU state, timers, MSRs,
XSAVE, virtual time, device bytes, and the CPU-contract hash. It has no host or
hypervisor dependencies.

## Format

Version 7 is a little-endian TLV container: a 10-byte header (magic, version,
architecture tag, and section count) followed by sections in ascending tag
order. X86 records always use the current SREGS and DEBUGREGS layouts with the
captured CPU fields (`flags` and `pdptrs`). The engine-state and
`xsave_restore_bv` and nested-state sections are optional within this current format. ARM uses
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
needs the value the guest was reading. ARM writes the selected counter directly
into its timer wire record; producing a hash encoding does not clone the snapshot
or its device payload. Both encoders validate section lengths and reserve the
complete output size once, so growing the output cannot repeatedly copy earlier
sections or leave capacity sized for an extra growth step. MSR and timer records
are written directly into that output, without temporary section buffers.

`peek_version` validates the magic and reads the version without decoding the
rest of the blob. `VM_STATE_VERSION` identifies the only writer and reader
format. The restore-bits section is validated for wire shape here; vmm-core owns
any backend-specific validation of the captured XSAVE image.

X86 tag 16 carries the entire variable-length KVM nested-state header and
payload. The codec checks the declared size and the supported 128–8320 byte
bound, and preserves every byte for identity and portable export. The backend
and VMM validate VMX format, host capability, contract, and capture boundary.

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

## Encoding qualification

The qualification compiles current and original encoding routines into one
release executable. The reference uses current record types, decoders, and
dependencies, with the encoding routines from `4ef653f1f` substituted from
`qualification/reference-*.rs`; its ARM hash path retains the full-state clone.
It compares complete bytes, decoded records, ARM counter normalization without
mutating the source, and rejection of invalid timer queues. Cases include empty
and populated optional records and device payloads from zero bytes to 1 MiB.
The VMM comparison substitutes only the codec dependency and checks full state
blobs, capture/restore, and state hashes for both mock architectures.

```sh
python3 consonance/vm-state/qualification/qualify-codec.py --check
python3 consonance/vm-state/qualification/qualify-codec.py
```

The full run reports nine alternating pairs using the normal system allocator;
every timed output is compared with the original encoder's bytes. Reported
latency includes that comparison. Output capacity is a buffer size, not RSS or
an allocation count. The VMM timing covers a 4 KiB mock VM with snapshot hashing
wired; it does not measure guest execution or hypervisor calls. The executable's
SHA-256 binds both arms to one build. Update the frozen encoding routines when
intentionally changing the wire format, keeping them independent of the optimized
implementation.

Nested state tag 16 retains the complete KVM vendor header and payload. VMX
uses format 0 and SVM format 1. SVM outside L2 carries a 128-byte header with
GIF; active L2 state can also include the 4 KiB VMCB. EFER and the native SVM
host-save/control MSRs remain in their architectural fields. The owning VMM
validates the format against the named contract before restore.

The generic property generators include absent, header-only, VMCS12/VMCB and
full VMCS12-plus-shadow nested payloads. Strict-decoder fixtures contain tag 16
and check its optionality, ordering, uniqueness, size field and truncation.
Native tests and whole-crate Miri check every truncated prefix. The Miri PR
smoke lane samples every section-header byte, each payload boundary and payload
interiors so the full nested fixture fits the lane's 15-minute budget.
