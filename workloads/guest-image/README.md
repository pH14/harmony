<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# guest-image

`guest-image` owns the deterministic `newc` cpio writer shared by host-side
guest image preparation and the OCI staging CLI. Entries are emitted with
zeroed ownership and timestamps, stable inode numbers, and sorted directory
walks so an identical input tree produces identical initramfs bytes.

The crate is intentionally small and Unix-oriented: its output is consumed by
the Linux guest image path, while callers remain responsible for compression
and for appending the segment to the chosen base initramfs.
