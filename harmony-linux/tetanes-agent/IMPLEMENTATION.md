# M2 arm64 TetaNES guest agent

This standalone guest crate runs `tetanes-core = 0.15.0` with the same
zero-RAM, headless, no-run-ahead configuration as
`dissonance/machine::nes::NesMachine`.

Each ordered payload entry is exactly `[buttons, hold_frames]`. The duration is
clamped to `1..=120`, all frames are emulated, the controller is released, all
2 KiB of WRAM are copied into one locked guest page, and only then does the
agent emit `frame_complete(cumulative_frames)`. The following payload fetch is
the synchronized boundary at which the VMM can seal that lifecycle yield.

The guest publishes the WRAM mirror's GPA and exact length through SDK state
registers before `setup_complete`. Linux/aarch64 maps the canonical 16-KiB
control slot (`REQ_GPA`/`RESP_GPA`) and the board's `0x0A00_0000` MMIO doorbell
through `/dev/mem`.

The live binary contains `unsafe` only at the OS/device-memory edges. All
input-dependent bounds and pagemap arithmetic live in the library and are
covered by ordinary tests and Miri; the actual `mmap` and volatile-MMIO paths
are architecture/OS-gated, like `harmony-linux/play-agent`.
