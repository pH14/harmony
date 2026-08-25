# The clippy gate is red on Linux with current stable, in four crates

Found while building this program's own crate on the box. Recorded, not fixed beyond the
one crate this program had to build and test there.

The box runs `clippy 0.1.98 (2026-08-18)`; the development host this program was driven
from runs `0.1.94 (2026-03-25)`. `rust-toolchain.toml` pins `channel = "stable"` with no
version, so the two are both "stable" and lint differently. `clippy::chunks_exact_to_as_chunks`
is in `clippy::all` on 1.98 and fires on five call sites:

| crate | site |
| --- | --- |
| snapshot-store | `src/lib.rs:130` `chunks_exact(8)` |
| vmm-backend | `src/kvm.rs:1020` `chunks_exact(4)` |
| vm-state | `src/codec.rs:317` `chunks_exact(12)` |
| vm-state | `src/codec.rs:400` `chunks_exact(32)` |
| cpu-qualification | `src/guest.rs:284` `chunks_exact(12)` |

Each is `-D warnings`, so the gate does not warn, it fails to compile.

Only the last one was changed here, because this program had to build, lint and test
`cpu-qualification` on the box and could not do that with the gate red. The other four are
outside what this program was asked to touch and each one is in a decoder where a reviewer
should look at the remainder handling, so they are left as they are.

Two things to decide, neither of them this program's to decide.

- Pin the toolchain. `rust-toolchain.toml` already carries a note saying to pin an exact
  stable version once a CI box exists. Until that happens, "the gate is green" means "green
  on whatever stable the person ran it with", and a macOS host on an older stable cannot
  see a Linux CI failure.
- The four remaining sites need one mechanical change each.
