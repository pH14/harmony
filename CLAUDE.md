# Project Instructions for AI Agents

Read `AGENTS.md` — it is the single standing-context file for this repo: what harmony is,
what "correct" means (determinism is the bar), the build/quality gates, review priorities,
conventions, and license rules.

Quick facts:

- Rust workspace; gates are `cargo build` / `cargo nextest run` / `cargo clippy -D warnings`
  / `cargo fmt --check` / `cargo deny check`, all `--all-features`, on macOS **and** Linux.
- Any crate containing `unsafe` must run clean under Miri.
- Track bugs and follow-ups in GitHub issues (`gh`), not markdown TODO lists.
- Do not commit or push unless asked.
