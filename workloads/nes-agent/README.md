<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# Portable NES play-agent

This allocation-only library owns the ordinary NES action loop, core interface,
SDK channel interface and publication catalog. Linux and WebAssembly guests use
the same `NesAgent` and `nes-protocol` action/observation semantics.

Setup publishes power-on RAM with no action frames. Each two-byte payload holds
a button mask for 1 through 120 frames, publishes every intermediate work-RAM
frame plus endpoint save RAM, then emits the shared SDK frame-complete event.
The library performs no game-specific interpretation or emulator-state encoding.
