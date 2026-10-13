<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Guest image support

`guest-image` owns the deterministic `newc` cpio writer used by host-side
guest image preparation and OCI staging. Entries are emitted with
zeroed timestamps, stable inode numbers, and sorted directory walks so an
identical input tree produces identical initramfs bytes.
Walked entry names must be valid UTF-8. A name that cannot be represented
exactly is rejected, and a failed tree walk leaves the writer unchanged;
replacement characters would otherwise collapse distinct Unix filenames into
the same guest path.

Each entry records the owner the caller supplies, which the kernel applies
when it unpacks the initramfs; `tree_owned` asks for every entry's owner by its
path relative to the walked root, and the plain entry points record root. The
on-disk owner of a staged tree is never read: it is whoever ran the staging.
`tree_owned_with_modes` also lets a caller provide each entry's permission bits
when the staging filesystem cannot preserve the intended guest mode.

The crate is intentionally small and Unix-oriented: its output is consumed by
the Linux guest image path, while callers remain responsible for compression
and for appending the segment to the chosen base initramfs.
