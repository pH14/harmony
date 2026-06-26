#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Provision the harmony dev box (Debian stable, bare-metal Intel).
# Idempotent: safe to re-run. Usage: git clone the repo, then run this as root.
set -euo pipefail

if [[ $(id -u) -ne 0 ]]; then echo "run as root" >&2; exit 1; fi

echo "== apt packages"
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y \
    build-essential git curl wget pkg-config \
    tmux mosh htop ripgrep jq \
    qemu-system-x86 \
    flex bison libelf-dev libssl-dev bc cpio kmod \
    shellcheck \
    linux-perf msr-tools cpuid \
    docker.io

echo "== rust (rustup, stable)"
if ! command -v rustup >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
fi
# shellcheck disable=SC1091
source "$HOME/.cargo/env"
grep -q 'cargo/env' "$HOME/.bashrc" || echo 'source "$HOME/.cargo/env"' >> "$HOME/.bashrc"
rustup toolchain install stable --component rustfmt,clippy
rustup target add x86_64-unknown-none

echo "== PMU / perf access for the precise-count work"
cat > /etc/sysctl.d/90-harmony.conf <<'EOF'
# Full perf_event access (PMU counting of guest execution needs it)
kernel.perf_event_paranoid = -1
# Don't lose PMU samples to the throttler during calibration
kernel.perf_event_max_sample_rate = 100000
EOF
sysctl --system >/dev/null

echo "== sanity checks"
fail=0
[[ -e /dev/kvm ]] || { echo "FAIL: /dev/kvm missing"; fail=1; }
grep -q vmx /proc/cpuinfo || { echo "FAIL: no VMX"; fail=1; }
command -v qemu-system-x86_64 >/dev/null || { echo "FAIL: qemu missing"; fail=1; }
cargo --version >/dev/null || { echo "FAIL: cargo missing"; fail=1; }
perf stat -e branches -- true >/dev/null 2>&1 || { echo "FAIL: perf counters unavailable"; fail=1; }
if [[ $fail -eq 0 ]]; then
    echo "provision OK: $(hostname) / $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2 | xargs)"
else
    exit 1
fi
