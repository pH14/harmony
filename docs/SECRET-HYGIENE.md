<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Credential hygiene

Harmony uses three independent defenses against publishing credentials:

1. `detect-secrets` scans staged files before each local commit.
2. TruffleHog scans every push and pull request in GitHub Actions, plus the complete
   repository history weekly and on manual dispatch.
3. GitHub secret scanning and push protection provide the server-side backstop.

No detector is complete. Never commit a real credential, even if a scanner accepts it.

## Local setup

Install the hook runner, configure the repository-managed hooks, and prefetch the pinned
scanner environment:

```sh
brew install pre-commit              # macOS; or: pipx install pre-commit
git config core.hooksPath .githooks
pre-commit install-hooks
```

Do not run `pre-commit install`: Harmony tracks `.githooks/pre-commit` so the hook behavior
is the same for every contributor. To scan every tracked file explicitly, run:

```sh
pre-commit run detect-secrets --all-files
```

The local hook disables online verification so a candidate value is not sent to a provider
from a developer workstation. A false positive may be annotated with the narrowest supported
`pragma: allowlist secret` comment, but only after confirming that the value is inert test data.
Never add a live credential to an allowlist or baseline. `git commit --no-verify` is an emergency
escape hatch, not an approval to bypass review.

## Where credentials belong

- Keep local values in `.env`, which the root `.gitignore` excludes. Commit only placeholder
  names in an `.env.example` file.
- Store CI values in GitHub Actions environment, repository, or organization secrets. Scope
  each value to the smallest set of jobs and environments that needs it.
- Prefer GitHub Actions OpenID Connect for cloud access so jobs receive short-lived credentials
  instead of storing long-lived cloud keys.
- Use a secret manager such as 1Password, Bitwarden, or Vault for shared developer credentials.
  Inject values at runtime; do not copy them into source, fixtures, issue text, or logs.

For public repositories, GitHub secret scanning runs automatically and user push protection is
enabled by default. Repository administrators should also enable repository push protection when
GitHub exposes it under the repository's code-security settings, and should not permit routine
bypasses.

## If a credential is committed

Treat a public commit as disclosure even if it is immediately deleted:

1. Revoke or rotate the credential first.
2. Determine its scope and inspect provider audit logs for misuse.
3. Remove the value from the working tree and use `git-filter-repo` when history cleanup is
   warranted; coordinate the rewrite with every clone and fork owner.
4. Contact GitHub Support when cached views, pull-request references, or forks keep the value
   reachable.
5. Record the incident without copying the credential into another system.

History rewriting is cleanup, not revocation: old clones and cached objects may retain the value.
