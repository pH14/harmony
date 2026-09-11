<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Harmony skills

This directory contains four application-independent Codex skills for Harmony:
deriving properties, adding SDK instrumentation, preparing reproducible build
artifacts, and running searches or durable investigations. They provide
workflow guidance only; workload-specific properties, oracles, inputs, and
strategies remain discoverable from the assigned source.

Install a skill by placing its complete directory (including `SKILL.md` and
optional `agents/openai.yaml`) under `$CODEX_HOME/skills` or `~/.codex/skills`.
