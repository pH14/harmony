# M2 arm64 TetaNES guest agent

This standalone guest crate runs `tetanes-core = 0.15.0` with the same
zero-RAM, headless, no-run-ahead configuration as
`dissonance/machine::nes::NesMachine`.

Each ordered payload entry is exactly `[buttons, hold_frames]`. The duration is
clamped to `1..=120`; frames are emulated through the hold or the first
observation-layer death/victory frame, matching the search target's endpoint
semantics. The controller is released, all 2 KiB of WRAM are copied into one
locked guest page, and only then does the agent emit
`frame_complete(cumulative_frames)`. Immediately after every lifecycle
event, the agent performs one volatile read of the board pvclock frame's ABI
register. That already-modeled MMIO access advances virtual_time V-time and gives
the VMM a synchronized boundary at which it can surface the deferred yield before
the agent fetches another payload.

The guest publishes the WRAM mirror's GPA and exact length through SDK state
registers before `setup_complete`. Linux/aarch64 maps the canonical 16-KiB
control slot (`REQ_GPA`/`RESP_GPA`) and the board's `0x0A00_0000` MMIO doorbell
through `/dev/mem`. It maps the `0x0B00_0000` pvclock register frame before
`setup_complete`, then requires ABI version 1 from the post-event reads. Mapping
before setup prevents an intervening syscall boundary from becoming the seal.

The death/victory checks are deliberately confined to this SMB observation
layer. They read only the same documented WRAM fields as the host target and do
not encode a route, level layout, obstacle, or input sequence. An early lifecycle
yield is valid only when those terminal bytes explain it; the control adapter
rejects an early non-terminal yield.

The live binary contains `unsafe` only at the OS/device-memory edges. All
input-dependent bounds and pagemap arithmetic live in the library and are
covered by ordinary tests and Miri; the actual `mmap` and volatile-MMIO paths
are architecture/OS-gated, like `harmony-linux/play-agent`.
