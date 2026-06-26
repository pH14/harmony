#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Provision (idempotently) the self-hosted GitHub Actions runner on the
# determinism box. This is the *record* of how the runner is set up, and re-runnable.
#
# What it sets up:
#   - a non-root `runner` user (CI build scripts must NOT run as root on the box that
#     also does determinism measurements), added to the `kvm` group;
#   - the `ci.slice` systemd cpuset — AllowedCPUs=5-7,13-15, i.e. cores 5/6/7 + SMT
#     siblings, deliberately OFF the measurement cores 2/4 (see docs/BOX-PINNING.md
#     "Self-hosted CI runner isolation"); every job the runner spawns inherits it;
#   - the actions runner (pinned version), registered to pH14/harmony, run as a
#     systemd service placed in ci.slice;
#   - the runner user's Rust toolchain (rustup + nightly + miri for the Miri gate,
#     quality-g §1; + the x86_64-unknown-none target for guest payloads).
#
# Secret: the runner registration token is EPHEMERAL (repo Settings -> Actions ->
# Runners -> New self-hosted runner; ~1h TTL, single-use, consumed on registration).
# It is the ONLY secret involved and is never stored by this script. The runner's own
# auto-managed `.credentials` (repo-scoped, instantly revocable by deleting the runner)
# is the single documented exception to "the box holds no credentials". Provide the
# token via the RUNNER_TOKEN env var, or place it in a gitignored ./.env as
# GITHUB_RUNNER=<token>. A runner that is already configured self-renews and needs no
# token, so re-running this to update the slice/toolchain needs no secret.
#
# Usage:  RUNNER_TOKEN=<token> DET_BOX_SSH=<ssh-alias> scripts/setup-ci-runner.sh
#         (run from the repo root on the Mac; it ssh-es to $DET_BOX_SSH)
set -euo pipefail

# DET_BOX_SSH is your determinism box's ssh alias (defined in ~/.ssh/config); no
# host is hard-coded here. REPO_URL is the public repo the runner registers to.
HOST="${DET_BOX_SSH:-det-box}"
REPO_URL="https://github.com/pH14/harmony"
RUNNER_NAME="det-box"
LABELS="self-hosted,linux,x64,kvm"
RUNNER_VERSION="2.335.1"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TOKEN="${RUNNER_TOKEN:-}"
if [ -z "$TOKEN" ] && [ -f "$here/../.env" ]; then
    TOKEN="$(grep -E '^GITHUB_RUNNER=' "$here/../.env" | cut -d= -f2- | tr -d "\"' " || true)"
fi

# The whole box-side procedure runs as one remote script; the token is passed as $1
# (so it never lands in a remote file). `bash -s` reads the heredoc on stdin.
ssh "$HOST" "bash -s -- '${TOKEN}' '${REPO_URL}' '${RUNNER_NAME}' '${LABELS}' '${RUNNER_VERSION}'" <<'REMOTE'
set -euo pipefail
TOKEN="$1"; REPO_URL="$2"; RUNNER_NAME="$3"; LABELS="$4"; RUNNER_VERSION="$5"

# 1. non-root runner user, in the kvm group (for any KVM-touching gate)
id runner >/dev/null 2>&1 || useradd -m -s /bin/bash runner
getent group kvm >/dev/null 2>&1 && usermod -aG kvm runner || true

# 2. ci.slice cpuset — OFF the measurement cores (docs/BOX-PINNING.md)
cat > /etc/systemd/system/ci.slice <<'SLICE'
[Unit]
Description=CI runner slice — cpuset-isolated off determinism measurement cores (docs/BOX-PINNING.md)
Before=slices.target
[Slice]
AllowedCPUs=5-7,13-15
SLICE
systemctl daemon-reload
systemctl start ci.slice

# 3. download + extract the runner (skip if present)
su - runner -c "set -e; mkdir -p ~/actions-runner; cd ~/actions-runner
  if [ ! -f ./config.sh ]; then
    curl -sL -o r.tgz https://github.com/actions/runner/releases/download/v${RUNNER_VERSION}/actions-runner-linux-x64-${RUNNER_VERSION}.tar.gz
    tar xzf r.tgz && rm r.tgz
  fi"

# 4. register (only if not already configured — token required only here)
if ! su - runner -c 'test -f ~/actions-runner/.runner'; then
    [ -n "$TOKEN" ] || { echo 'ERROR: runner not configured and no registration token provided' >&2; exit 1; }
    su - runner -c "cd ~/actions-runner && ./config.sh --url '$REPO_URL' --token '$TOKEN' \
        --name '$RUNNER_NAME' --labels '$LABELS' --unattended --replace"
else
    echo '== runner already configured; skipping registration (it self-renews its credential)'
fi

# 5. systemd service, placed in ci.slice
cd /home/runner/actions-runner
systemctl list-unit-files 'actions.runner.*' --no-legend 2>/dev/null | grep -q . || ./svc.sh install runner
SVC="$(systemctl list-unit-files 'actions.runner.*' --no-legend | awk '{print $1}' | head -1)"
mkdir -p "/etc/systemd/system/${SVC}.d"
printf '[Service]\nSlice=ci.slice\n' > "/etc/systemd/system/${SVC}.d/slice.conf"
systemctl daemon-reload
systemctl restart "$SVC"

# 6. runner user's Rust toolchain (rustup + nightly+miri for quality-g §1; + bare-metal target)
su - runner -c 'set -e
  command -v rustup >/dev/null 2>&1 || curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain none >/dev/null
  source ~/.cargo/env
  rustup toolchain install nightly --component miri >/dev/null 2>&1 || rustup toolchain install nightly --component miri
  rustup target add x86_64-unknown-none >/dev/null 2>&1 || true
  echo "== toolchain: $(rustup toolchain list | tr "\n" " ")"
  # CI cargo tools — pre-provisioned here (NOT per-job via taiki-e/install-action, which
  # raced on a shared temp across the concurrent runners). cargo-binstall fetches the
  # prebuilt binary; idempotent (skips if already on PATH). Keep this list in sync with
  # the tools the quality.yml jobs invoke.
  command -v cargo-binstall >/dev/null 2>&1 || curl -L --proto "=https" --tlsv1.2 -sSf https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh | bash >/dev/null
  for t in cargo-nextest cargo-deny cargo-llvm-cov cargo-mutants cargo-public-api; do
    command -v "$t" >/dev/null 2>&1 || cargo binstall -y "$t" >/dev/null 2>&1 || cargo binstall -y "$t"
  done
  echo "== ci tools: $(for t in cargo-nextest cargo-deny cargo-llvm-cov cargo-mutants cargo-public-api; do command -v "$t" >/dev/null 2>&1 && echo -n "$t " ; done)"'

# 7. put cargo on every job's PATH. A GitHub Actions job runs in a NON-login shell that
#    does NOT source ~/.cargo/env, and the runner's `.path` file is the base PATH it gives
#    jobs — so unless cargo-bin is on `.path`, every job dies with `rustup: command not
#    found` (exit 127). Prepend /home/runner/.cargo/bin to `.path` (idempotent), then
#    restart so the runner re-reads it. (add-ci-runners.sh copies this `.path` to the extra
#    concurrent runners, so fixing it here propagates.)
su - runner -c 'cd ~/actions-runner
  base="$(cat .path 2>/dev/null || echo "/usr/local/bin:/usr/bin:/bin:/usr/local/games:/usr/games")"
  case ":$base:" in
    *":/home/runner/.cargo/bin:"*) : ;;
    *) printf "%s" "/home/runner/.cargo/bin:$base" > .path ;;
  esac
  echo "== runner .path: $(cat .path)"'
systemctl restart "$SVC"

echo "== runner: $(systemctl is-active "$SVC"), slice=$(systemctl show "$SVC" -p Slice --value), AllowedCPUs=$(systemctl show ci.slice -p AllowedCPUs --value)"
REMOTE
echo "done."
