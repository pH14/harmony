<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Acceptance goldens

This directory stores reviewed outputs for the payloads in `../payloads/`.

The `.txt` files are serial-shape goldens used by the portable QEMU gate. The
`.digest` files are observation digests captured by the hardware-backed corpus
gate. A digest includes the report stream and the serial payload output; it is
not interchangeable with its serial-shape file.

`*.provenance.md` files record the external inputs and capture context needed
to audit a hardware-derived digest. They are evidence for the corresponding
golden, not implementation logs.

When a payload or deterministic contract changes, regenerate the relevant
golden using its owning gate, inspect the complete diff, and update adjacent
provenance when the hardware digest changes. Digest values come from the
owning gate rather than manual edits.
