#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build, attest, run, and replay the complete M2 Apple-HVF SMB campaign oracle.
set -euo pipefail

if [[ $# -lt 5 || $# -gt 6 ]]; then
    echo "usage: $0 <Image-game> <initramfs-game.cpio.gz> <rom> <rom-sha256-sidecar> <output-dir> [execution-budget]" >&2
    exit 2
fi

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd "$script_dir/../.." && pwd)

# shellcheck source=lib.sh disable=SC1091
. "$script_dir/lib.sh"

image=$1
initramfs=$2
rom=$3
rom_sidecar=$4
output_dir=$5
execution_budget=${6:-4096}
campaign_seed=0x5eedca22
minimum_continuation_restores=2000
continuation_samples=32

if [[ $(uname -s) != Darwin || $(uname -m) != arm64 ]]; then
    echo "FAIL: the M2 live oracle requires an Apple Silicon macOS host" >&2
    exit 1
fi
for tool in cargo codesign cmp grep python3; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "FAIL: missing required host tool: $tool" >&2
        exit 1
    fi
done
for artifact in "$image" "$initramfs" "$rom" "$rom_sidecar"; do
    if [[ ! -s "$artifact" ]]; then
        echo "FAIL: required M2 artifact is missing or empty: $artifact" >&2
        exit 1
    fi
done
if [[ ! "$execution_budget" =~ ^[0-9]+$ ]] \
    || (( execution_budget < minimum_continuation_restores )); then
    echo "FAIL: execution budget must be an integer >= $minimum_continuation_restores" >&2
    exit 1
fi
if [[ -e "$output_dir" ]]; then
    echo "FAIL: output directory already exists: $output_dir" >&2
    exit 1
fi

rom_sha=$(sha256_of "$rom")
if [[ $(wc -l <"$rom_sidecar" | tr -d ' ') != 1 ]]; then
    echo "FAIL: ROM sidecar must contain exactly one digest line" >&2
    exit 1
fi
IFS= read -r embedded_rom_sha <"$rom_sidecar"
if [[ ! "$embedded_rom_sha" =~ ^[0-9a-f]{64}$ ]]; then
    echo "FAIL: ROM sidecar is not one lowercase SHA-256 digest" >&2
    exit 1
fi
if [[ "$rom_sha" != "$embedded_rom_sha" ]]; then
    echo "FAIL: host ROM differs from the ROM embedded in the initramfs" >&2
    exit 1
fi

mkdir -p "$output_dir"
oracle_tmp=$(mktemp -d "${TMPDIR:-/tmp}/harmony-m2-oracle.XXXXXX")
server_pid=
cleanup() {
    if [[ -n "$server_pid" ]] && kill -0 "$server_pid" 2>/dev/null; then
        kill "$server_pid" 2>/dev/null || true
        wait "$server_pid" 2>/dev/null || true
    fi
    rm -rf -- "$oracle_tmp"
}
trap cleanup EXIT

echo "== M2: building production compositions"
(cd "$repo_root" && cargo build --release -p vmm-core --bin hvf_control_server)
(cd "$repo_root" && cargo build --release --manifest-path dissonance/Cargo.toml \
    -p searcher --bin smb-campaign --bin smb-vtime-continuation)
server=$repo_root/target/release/hvf_control_server
searcher=$repo_root/dissonance/target/release/smb-campaign
continuation_oracle=$repo_root/dissonance/target/release/smb-vtime-continuation
entitlements=$repo_root/consonance/vmm-backend/hvf.entitlements.plist
for artifact in "$server" "$searcher" "$continuation_oracle" "$entitlements"; do
    if [[ ! -s "$artifact" ]]; then
        echo "FAIL: built M2 artifact is missing or empty: $artifact" >&2
        exit 1
    fi
done

codesign --force --sign - --entitlements "$entitlements" "$server"
codesign --verify --strict "$server"
signed_entitlements=$output_dir/hvf-control-server.entitlements.plist
codesign -d --entitlements :- "$server" >"$signed_entitlements"
if ! grep -q '<key>com.apple.security.hypervisor</key>' "$signed_entitlements"; then
    echo "FAIL: signed control server lacks the Hypervisor.framework entitlement" >&2
    exit 1
fi

{
    printf '%s  Image-game\n' "$(sha256_of "$image")"
    printf '%s  initramfs-game.cpio.gz\n' "$(sha256_of "$initramfs")"
    printf '%s  smb.nes\n' "$rom_sha"
    printf '%s  hvf_control_server.signed\n' "$(sha256_of "$server")"
    printf '%s  smb-campaign\n' "$(sha256_of "$searcher")"
    printf '%s  smb-vtime-continuation\n' "$(sha256_of "$continuation_oracle")"
} >"$output_dir/MANIFEST.sha256"

wait_for_socket() {
    local socket=$1
    local attempts=0
    while [[ ! -S "$socket" ]]; do
        if ! kill -0 "$server_pid" 2>/dev/null; then
            echo "FAIL: HVF control server exited before binding $socket" >&2
            return 1
        fi
        attempts=$((attempts + 1))
        if (( attempts >= 300 )); then
            echo "FAIL: HVF control server did not bind $socket within 30 seconds" >&2
            return 1
        fi
        sleep 0.1
    done
}

start_server() {
    local label=$1
    local sessions=$2
    current_socket=$oracle_tmp/$label.sock
    current_server_stdout=$output_dir/$label-server.stdout
    current_server_stderr=$output_dir/$label-server.stderr
    "$server" "$image" "$initramfs" "$current_socket" "$sessions" \
        >"$current_server_stdout" 2>"$current_server_stderr" &
    server_pid=$!
    wait_for_socket "$current_socket"
}

finish_server() {
    local expected_sessions=$1
    wait "$server_pid"
    server_pid=
    if ! grep -q '^HVF_CONTROL_SERVER_READY ' "$current_server_stdout"; then
        echo "FAIL: HVF control server emitted no readiness record" >&2
        return 1
    fi
    local completed
    completed=$(grep -c '^HVF_CONTROL_SESSION_OK ' "$current_server_stdout" || true)
    if [[ "$completed" != "$expected_sessions" ]]; then
        echo "FAIL: control server completed $completed sessions, expected $expected_sessions" >&2
        return 1
    fi
    if grep -a -Eiq 'watchdog|TETANES_GAME_SKIP|panic|FAIL:' \
        "$current_server_stdout" "$current_server_stderr"; then
        echo "FAIL: control server reported a watchdog, skipped payload, panic, or failure" >&2
        return 1
    fi
}

run_campaign() {
    local label=$1
    local run_dir=$output_dir/$label
    start_server "$label" 2
    HARMONY_SMB_ROM="$rom" "$searcher" run genesis "$campaign_seed" 1 \
        "$execution_budget" 96 m1-max "$run_dir" --control-socket "$current_socket" \
        >"$output_dir/$label-searcher.stdout" \
        2>"$output_dir/$label-searcher.stderr"
    finish_server 2
}

echo "== M2: running two same-seed production campaigns"
run_campaign run-1
run_campaign run-2

for artifact in archive-live.json campaign-report.json stream.jsonl snapshots-live.bin; do
    cmp -s "$output_dir/run-1/$artifact" "$output_dir/run-2/$artifact" || {
        echo "FAIL: same-seed campaigns differ in $artifact" >&2
        exit 1
    }
done
archive_sha=$(sha256_of "$output_dir/run-1/archive-live.json")

read -r genesis_restores continuation_restores executions_completed archive_entries < <(
    python3 -c 'import json,sys; r=json.load(open(sys.argv[1])); print(r["snapshot_restores"]["genesis"], r["snapshot_restores"]["continuation"], r["executions_completed"], len(r["archive"]["entries"]))' \
        "$output_dir/run-1/campaign-report.json"
)
if (( genesis_restores < 1 )); then
    echo "FAIL: campaign report recorded no genesis restore" >&2
    exit 1
fi
if (( continuation_restores < minimum_continuation_restores )); then
    echo "FAIL: campaign recorded only $continuation_restores continuation restores" >&2
    exit 1
fi
if (( executions_completed != execution_budget )); then
    echo "FAIL: campaign completed $executions_completed jobs, expected $execution_budget" >&2
    exit 1
fi
if (( archive_entries < 1 )); then
    echo "FAIL: campaign retained no archive entries" >&2
    exit 1
fi

echo "== M2: sampling uninterrupted/restored continuation hashes"
start_server continuation 1
HARMONY_SMB_ROM="$rom" "$continuation_oracle" "$current_socket" \
    "$output_dir/run-1/snapshots-live.bin" "$continuation_samples" \
    "$output_dir/continuation-hashes.json" \
    >"$output_dir/continuation-searcher.stdout" \
    2>"$output_dir/continuation-searcher.stderr"
finish_server 1
read -r sampled_branch_points chord_hashes_compared < <(
    python3 -c 'import json,sys; r=json.load(open(sys.argv[1])); print(r["sampled_branch_points"], r["chord_hashes_compared"])' \
        "$output_dir/continuation-hashes.json"
)
if (( sampled_branch_points != continuation_samples )); then
    echo "FAIL: sampled $sampled_branch_points branch points, expected $continuation_samples" >&2
    exit 1
fi
if (( chord_hashes_compared < continuation_samples )); then
    echo "FAIL: only $chord_hashes_compared continuation chord hashes were compared" >&2
    exit 1
fi

echo "== M2: replaying the complete retained campaign through a fresh VM"
start_server replay 1
HARMONY_SMB_ROM="$rom" "$searcher" replay "$output_dir/run-1" genesis \
    --control-socket "$current_socket" \
    >"$output_dir/replay-searcher.stdout" \
    2>"$output_dir/replay-searcher.stderr"
finish_server 1
python3 -c 'import json,sys; assert json.load(open(sys.argv[1]))["replay_verified"] is True' \
    "$output_dir/run-1/replay-verdict.json"

cat >"$output_dir/M2-ORACLE.txt" <<EOF
M2_CAMPAIGN_ORACLE_OK
campaign_seed=$campaign_seed
execution_budget=$execution_budget
archive_sha256=$archive_sha
archive_entries=$archive_entries
genesis_restores=$genesis_restores
continuation_restores=$continuation_restores
sampled_branch_points=$sampled_branch_points
continuation_chord_hashes=$chord_hashes_compared
same_seed_archives=2
fresh_vm_replay_verified=true
EOF
cat "$output_dir/MANIFEST.sha256"
cat "$output_dir/M2-ORACLE.txt"
