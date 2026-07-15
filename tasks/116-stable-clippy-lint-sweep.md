# tasks/116 — Stable-bump clippy lint sweep (CI green restoration)

**Context:** The CI runner's corrupted stable toolchain was reinstalled 2026-07-15 eve,
which pulled the CURRENT stable — newer than the previous install. Its new clippy lints
now fail `-D warnings` on existing main code: first bailure is `clippy::byte_char_slices`
in `control-proto` (run 29458561472, job `gates`). There may be more sites/lints behind
it (the build bailed early).

**Task (narrow, mechanical):**
1. On the box (`ssh hetzner`, as your normal task flow — or locally IF your Mac's stable
   matches the runner's new stable version; check `rustup run stable rustc --version`
   both sides and say which you used): run the gates job's clippy invocation across the
   workspace and collect EVERY new-lint failure.
2. Fix each mechanically in the idiomatic direction the lint suggests (e.g.
   `&[b'a', b'b']` → `b"ab"`). No `#[allow]`s unless a lint is genuinely wrong for the
   code — justify any allow in the PR body.
3. Full portable gates: workspace nextest, clippy -D warnings (all targets), fmt, deny.
   Wire-format-adjacent crates (control-proto!) must show zero behavior change — if a
   fix touches encoded bytes rather than just literals' spelling, STOP and escalate.
4. PR with the lint list + before/after for each site.

**Non-goals:** no toolchain pinning policy change (foreman/Paul decision, noted
separately); no drive-by refactors; nothing beyond what the new stable's lints demand.
