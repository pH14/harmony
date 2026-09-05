<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# tetanes-agent

The arm64 TetaNES agent runs the same headless, zero-RAM, no-run-ahead emulator
configuration used by the in-process NES machine. The host supplies an ordered
stream of two-byte entries, `[buttons, hold_frames]`; each hold is clamped to
`1..=120` frames.

After each entry, the agent releases the controller, copies the complete 2 KiB
WRAM window into the published guest page, and emits the cumulative frame count
through the SDK. A death or victory observation ends the current hold early.
The host can therefore snapshot at a frame-complete boundary before the next
entry is fetched.

The image-specific binary maps the control pages, doorbell, and arm64 pvclock
frame through `/dev/mem`. Bounds checking, WRAM decoding, and pagemap arithmetic
are kept in the library; those functions are tested independently of the
device-memory edges.
