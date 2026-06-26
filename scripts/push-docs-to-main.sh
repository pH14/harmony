#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Safe direct-to-main push, restricted to docs / specs / skills / feedback.
#
# Why this exists: `git push origin main` is content-blind, so it can't be
# safely allowlisted on its own — it would permit pushing unreviewed crate
# code straight to main. This wrapper mechanizes the foreman ground rule
# ("direct-to-main only for docs/specs/feedback/skills, never crate code"):
# it refuses to push if ANY pending commit touches a path outside the
# allowlist, so the allowlisted invocation is provably safe.
#
# Setup: add to .claude/settings.json (or settings.local.json) so this exact
# script auto-approves while raw `git push origin main` stays classifier-gated:
#   "permissions": { "allow": ["Bash(scripts/push-docs-to-main.sh:*)"] }
#
# Anything NOT allowlisted (consonance/**, dissonance/**, Cargo.toml,
# Cargo.lock, deny.toml, clippy.toml, .github/**, .claude/settings*, scripts/**, …)
# still requires a PR — fail-closed by default.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# Must be on main, plain fast-forward push only (never --force from here).
[ "$(git branch --show-current)" = "main" ] || { echo "refuse: not on main" >&2; exit 1; }
git fetch -q origin main

# Paths permitted to go direct to main. Everything else => open a PR.
ALLOW='^(docs/|tasks/|\.claude/skills/|feedback/|[^/]*\.md$)'

pending="$(git diff --name-only origin/main..HEAD)"
[ -n "$pending" ] || { echo "nothing to push (main is at or behind origin)"; exit 0; }

bad="$(printf '%s\n' "$pending" | grep -vE "$ALLOW" || true)"
if [ -n "$bad" ]; then
  echo "REFUSE: these pending paths are not docs/specs/skills — open a PR instead:" >&2
  printf '  %s\n' $bad >&2
  exit 1
fi

echo "safe — docs/specs/skills only:"; printf '  %s\n' $pending
git push origin main
