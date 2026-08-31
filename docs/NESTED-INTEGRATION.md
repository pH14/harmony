# Nested integration

Consonance requires only a hardware-virtualization surface capable of entering
the guest and returning its modeled exits. The exit-count virtual clock has no
host performance-counter or frequency dependency, so nested virtualization is
the same execution model as bare metal.

The outer hypervisor is trusted to preserve the advertised ISA and
virtualization ABI. Consonance still freezes the guest-visible CPU/register
surface, hides or traps nondeterministic instructions, uses a seeded entropy
stream, and assigns time solely from normalized exits.

Nested support is accepted with the same evidence as bare metal: two same-seed
runs must produce identical normalized logs and checkpoint hashes, snapshot
restore must be a fixpoint, and a planted mismatch must fail the comparator.
PR #235 records the completed x86 GitHub-hosted runner evidence, with the final
exact-tree execution preserved in
[Actions run 33343244890](https://github.com/pH14/harmony/actions/runs/33343244890).
Untested cells remain marked as such in [`DETERMINISM.md`](DETERMINISM.md).
