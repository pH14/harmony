#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Add N concurrent self-hosted CI runners on the determinism box so the quality-gate jobs
# (build/test/clippy/Miri/coverage/mutants/kani) run in parallel instead of serializing
# on a single runner. Companion to setup-ci-runner.sh (which provisions the first runner).
#
# Determinism safety: every added runner's service is placed in the **ci.slice** cpuset
# (AllowedCPUs=5-7,13-15 — cores 5/6/7 + SMT siblings, OFF the measurement cores 2/4 per
# docs/BOX-PINNING.md). Concurrent CI jobs therefore stay confined to the CI cores and
# never perturb a pinned determinism run on cores 2/4 (or the OS on core 0).
#
# Each added runner is a fresh extraction of the version-matched runner tarball, gets the
# cargo-bin PATH (the `.path` fix, copied from the base runner), and a fresh single-use
# registration token (minted here via `gh api`, passed positionally to one remote script —
# never stored on the box, never echoed). A configured runner self-renews its own
# repo-scoped `.credentials` thereafter (the single documented box credential, revocable
# by deleting the runner on GitHub).
#
# Usage:  scripts/add-ci-runners.sh [N]     # N additional runners (default 3 => 4 total)
#         Run from the repo root on the Mac; needs `gh` with runner-admin on the repo.
set -euo pipefail

# DET_BOX_SSH is your determinism box's ssh alias (~/.ssh/config); no host is
# hard-coded. REPO is the public repo the extra runners register to.
HOST="${DET_BOX_SSH:-det-box}"
REPO="pH14/harmony"
REPO_URL="https://github.com/$REPO"
LABELS="self-hosted,linux,x64,kvm"
BASE="/home/runner/actions-runner"
RUNNER_VERSION="2.335.1"
N="${1:-3}"

echo "Minting $N runner registration tokens…"
TOKENS=()
for _ in $(seq 1 "$N"); do
  t="$(gh api -X POST "repos/$REPO/actions/runners/registration-token" -q .token)"
  [ "${#t}" -gt 20 ] || { echo "failed to mint a registration token (need gh runner-admin on $REPO)" >&2; exit 1; }
  TOKENS+=("$t")
done

# One remote root script; tokens are positional args so they never hit a remote file.
ssh "$HOST" "sudo bash -s -- '$REPO_URL' '$LABELS' '$BASE' '$RUNNER_VERSION' '$N' ${TOKENS[*]}" <<'REMOTE'
set -euo pipefail
REPO_URL="$1"; LABELS="$2"; BASE="$3"; RUNNER_VERSION="$4"; N="$5"; shift 5
TOKENS=("$@")

[ -d "$BASE" ] || { echo "base runner $BASE missing — run setup-ci-runner.sh first" >&2; exit 1; }
systemctl cat ci.slice >/dev/null 2>&1 || { echo "ci.slice missing — run setup-ci-runner.sh first" >&2; exit 1; }

# Download the version-matched tarball once; extract a fresh (unconfigured) copy per runner.
TGZ="/tmp/actions-runner-${RUNNER_VERSION}.tgz"
[ -f "$TGZ" ] || curl -sL -o "$TGZ" \
  "https://github.com/actions/runner/releases/download/v${RUNNER_VERSION}/actions-runner-linux-x64-${RUNNER_VERSION}.tar.gz"

for k in $(seq 0 $((N-1))); do
  i=$((k + 2))                               # base runner is #1; new ones start at 2
  NAME="det-box-$i"
  DIR="/home/runner/actions-runner-$i"
  SVC="actions.runner.pH14-harmony.${NAME}.service"
  TOKEN="${TOKENS[$k]}"

  if [ -f "$DIR/.runner" ]; then
    echo "runner $i already configured — ensuring service is started"
    ( cd "$DIR" && ./svc.sh start >/dev/null 2>&1 || true )
    continue
  fi

  echo "=== adding runner $i ($NAME) ==="
  rm -rf "$DIR"
  sudo -u runner mkdir -p "$DIR"
  sudo -u runner tar xzf "$TGZ" -C "$DIR"

  sudo -u runner bash -c "cd '$DIR' && ./config.sh --unattended --replace \
      --url '$REPO_URL' --token '$TOKEN' --name '$NAME' --labels '$LABELS' --work _work" >/dev/null

  # config.sh regenerates `.path` (the base job PATH) from the configuring shell, WITHOUT
  # cargo-bin — so prepend it now, AFTER config, or every job dies `rustup: command not
  # found` (exit 127). Same fix as setup-ci-runner.sh step 7; must run post-config.
  p="$(cat "$DIR/.path" 2>/dev/null || echo "/usr/local/bin:/usr/bin:/bin")"
  case ":$p:" in
    *":/home/runner/.cargo/bin:"*) : ;;
    *) printf "%s" "/home/runner/.cargo/bin:$p" | sudo -u runner tee "$DIR/.path" >/dev/null ;;
  esac

  ( cd "$DIR" && ./svc.sh install runner )
  mkdir -p "/etc/systemd/system/${SVC}.d"
  printf '[Service]\nSlice=ci.slice\n' > "/etc/systemd/system/${SVC}.d/slice.conf"
  systemctl daemon-reload
  ( cd "$DIR" && ./svc.sh start )
  sleep 1
  echo "runner $i: $(systemctl is-active "$SVC") (ci.slice)"
done

echo "=== all runner services ==="
systemctl list-units 'actions.runner.*' --no-legend --plain | awk '{print $1, $3, $4}'
echo "=== ci.slice cpuset ==="; systemctl show ci.slice -p AllowedCPUs
REMOTE

echo
echo "Done. GitHub → Settings → Actions → Runners should now list $((N+1)) runners (all 'self-hosted')."
