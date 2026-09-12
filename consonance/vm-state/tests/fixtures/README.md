<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# vm-state fixtures

The immutable `GOLDEN_HEX` in `tests/golden.rs` is the matching x86 v3 fixture
from the same pre-v5 writer and remains the primary v3 byte-stability check.

`x86-extended-record.hex` is an immutable x86 v4 blob emitted by the
pre-v5 `vm-state` writer at commit `c950d4972df8a8f433f2a7b1fe90f4b08c426504`.
It contains the engine payload `[0xca, 0xfe]` and the old SREGS/DEBUGREGS
layouts. The golden test decodes it and requires the new writer to reproduce
the bytes exactly; the newly added CPU fields must therefore decode to zero.
