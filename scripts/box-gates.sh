#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# box-gates.sh — the packaged runner for the x86 hardware test suite
# (docs/TESTING.md, rungs 2/3/5 on the determinism box).
#
# ============================================================================
#  TRANSCRIBED, PENDING ITS FIRST HARDWARE RUN.
#
#  Every invocation below is transcribed from the corresponding test file's
#  own header and from the runbooks in the crates' IMPLEMENTATION.md. None of
#  them has yet been executed *through this script*; the first hardware run is
#  expected to correct details (timeouts, image staging, which gates a given
#  window can afford). What is not provisional is the ORDERING and the
#  MODULE DISCIPLINE below — both are load-bearing and were previously carried
#  only in prose.
# ============================================================================
#
# Why this exists. The ~65 #[ignore]d hardware test functions across ~26 files
# were each invoked by a hand-typed command recorded in a file header. That is
# not a suite: it cannot be run as a unit, its ordering constraints live in
# comments, and its "always revert the modules afterwards" rule is a sentence
# someone has to remember. This script is the single sequenced entry point, and
# each rule it encodes is a checked command rather than a note.
#
# Usage:
#   scripts/box-gates.sh                     # every gate, in order
#   scripts/box-gates.sh --list              # what would run, in order
#   scripts/box-gates.sh contract kvm-smoke  # only the named gates
#   scripts/box-gates.sh --skip pvclock      # everything except these
#   DRY_RUN=1 scripts/box-gates.sh           # print commands, run nothing
#
# Environment:
#   CORE_PIN_*     override a gate's pinned core (see the table below)
#   STOCK_KVM_SIZE the byte size `lsmod` must report for a stock kvm module
#                  after the revert (default 1396736 on the det-cfl-v1 box)
#   DRY_RUN=1      print each command instead of running it
#
# Fail-loud, never skip: a missing /dev/kvm, a missing patched module, or a
# missing guest image aborts with what is absent. A hardware gate that "passes"
# because it found nothing to run is the failure mode this whole apparatus
# exists to prevent (docs/TESTING.md, "hardware gates must fail loudly when the
# host baseline is missing").
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

STOCK_KVM_SIZE="${STOCK_KVM_SIZE:-1396736}"
DRY_RUN="${DRY_RUN:-0}"

# ---------------------------------------------------------------------------
# Core pinning. Cores are a leased resource on the box; these are the defaults
# each test header records. Override per gate with the matching CORE_PIN_*.
# ---------------------------------------------------------------------------
CORE_PIN_SMOKE="${CORE_PIN_SMOKE:-1}"        # kvm-smoke, contract, m1-m2, linux-boot
CORE_PIN_MEASURE="${CORE_PIN_MEASURE:-2}"    # determinism, preemption, pvclock, dirty-remap
CORE_PIN_FRONTIER="${CORE_PIN_FRONTIER:-4}"  # snapshot/branch, postgres, nonquiescent

# ---------------------------------------------------------------------------
# Loud helpers.
# ---------------------------------------------------------------------------
die() {
    echo "box-gates: FATAL: $*" >&2
    exit 1
}

note() { echo "box-gates: $*"; }

run() {
    if [[ "${DRY_RUN}" == "1" ]]; then
        echo "  DRY_RUN + $*"
        return 0
    fi
    echo "  + $*"
    "$@"
}

# ---------------------------------------------------------------------------
# Host baseline. Every check here is a precondition the gates silently assume;
# checking them once, loudly, up front beats discovering them as a confusing
# mid-gate failure.
# ---------------------------------------------------------------------------
require_host_baseline() {
    if [[ "${DRY_RUN}" == "1" ]]; then
        note "DRY_RUN: skipping the host-baseline check so the command sequence can be reviewed \
off-box. A real run checks it."
        return 0
    fi
    [[ "$(uname -s)" == "Linux" ]] || die "not Linux: the x86 hardware suite runs on the determinism box"
    [[ "$(uname -m)" == "x86_64" ]] || die "not x86-64: this is the x86 lane (the arm64 lane is separate)"
    [[ -e /dev/kvm ]] || die "/dev/kvm missing: this host is not a virtualization substrate"
    [[ -r /dev/kvm && -w /dev/kvm ]] || die "/dev/kvm is not readable+writable by this user"
    command -v taskset >/dev/null 2>&1 || die "taskset missing: core pinning is not optional here"
    command -v cargo >/dev/null 2>&1 || die "cargo missing (see scripts/provision-host.sh)"
    [[ -r /proc/sys/kernel/perf_event_paranoid ]] \
        || die "perf_event unavailable: run_until and the work counter need it"
    note "host baseline ok: $(uname -srm), /dev/kvm present, taskset present"
}

# The size `lsmod` reports for the stock kvm module. Used both to detect the
# patched modules and to verify the revert.
kvm_module_size() {
    awk '$1 == "kvm" { print $2 }' /proc/modules | head -n1
}

kvm_intel_users() {
    awk '$1 == "kvm_intel" { print $3 }' /proc/modules | head -n1
}

require_patched_modules() {
    if [[ "${DRY_RUN}" == "1" ]]; then
        note "DRY_RUN: assuming the patched KVM modules are loaded"
        return 0
    fi
    local size
    size="$(kvm_module_size || true)"
    [[ -n "${size}" ]] || die "the kvm module is not loaded at all"
    if [[ "${size}" == "${STOCK_KVM_SIZE}" ]]; then
        die "stock KVM is loaded (kvm size ${size}); this gate needs the PATCHED modules \
(KVM_CAP_X86_DETERMINISTIC_INTERCEPTS). Load them, then re-run."
    fi
    note "patched KVM modules detected (kvm size ${size}, stock is ${STOCK_KVM_SIZE})"
}

require_stock_modules() {
    if [[ "${DRY_RUN}" == "1" ]]; then
        note "DRY_RUN: assuming stock KVM is loaded"
        return 0
    fi
    local size
    size="$(kvm_module_size || true)"
    [[ "${size}" == "${STOCK_KVM_SIZE}" ]] \
        || die "expected stock KVM (kvm size ${STOCK_KVM_SIZE}), found '${size:-<unloaded>}'"
    note "stock KVM confirmed (kvm size ${size})"
}

# Return the host to stock KVM and VERIFY it. Every patched-module gate must
# leave the box on stock — the box is shared, and a patched module left loaded
# silently changes the meaning of the next person's run.
#
# The order matters and is transcribed from the test headers: kill any live
# harness FIRST, wait for kvm_intel's user count to reach zero, then unload and
# reload, then verify.
revert_to_stock_kvm() {
    note "reverting to stock KVM"
    if [[ "${DRY_RUN}" == "1" ]]; then
        echo "  DRY_RUN + wait for kvm_intel users == 0; rmmod kvm_intel kvm; modprobe kvm kvm_intel; verify size ${STOCK_KVM_SIZE}"
        return 0
    fi
    local users deadline
    deadline=$((SECONDS + 120))
    while :; do
        users="$(kvm_intel_users || echo 0)"
        [[ "${users:-0}" == "0" ]] && break
        (( SECONDS < deadline )) || die "kvm_intel still has ${users} user(s) after 120s: a VM is \
still live. Stop it before reverting — never rmmod under a running guest."
        sleep 2
    done
    run sudo rmmod kvm_intel
    run sudo rmmod kvm
    run sudo modprobe kvm
    run sudo modprobe kvm_intel
    if [[ "${DRY_RUN}" != "1" ]]; then
        require_stock_modules
    fi
}

# Run a gate's cargo invocation, reverting the modules afterwards whether it
# passed or failed. `trap` inside a function body would fight the outer script,
# so the revert is explicit and the exit status is preserved by hand.
with_stock_revert() {
    local rc=0
    "$@" || rc=$?
    revert_to_stock_kvm
    return "${rc}"
}

# ---------------------------------------------------------------------------
# The gates, in order. Each entry is `name|description|function`.
#
# ORDERING IS LOAD-BEARING, twice over:
#
#   1. Cheap-and-broad before slow-and-narrow. `kvm-smoke` and `contract`
#      answer "is this host a lawful backend at all" in seconds; running a
#      four-hour workload gate first only to fail on a broken substrate wastes
#      the window.
#   2. `pvclock` runs its g0 smoke FIRST, as its own gate. g0 is the
#      minutes-long probe of the riskiest live assumptions (does the kernel
#      build, does the guest register the clock page, does it read sane time,
#      does it reach readiness). Spending the G1/perf budget before g0 passes
#      burns hours on a wedged image. This is the one ordering the pvclock file
#      states explicitly; it is encoded here rather than left to memory.
# ---------------------------------------------------------------------------

gate_kvm_smoke() {
    require_stock_modules
    run taskset -c "${CORE_PIN_SMOKE}" \
        cargo test -p vmm-backend --test kvm_smoke -- --ignored --test-threads=1
}

gate_contract() {
    # Rung 2: the shared Backend contract exam over the live backends. The
    # stock leg needs only /dev/kvm; the patched leg needs the patched modules,
    # so the whole gate runs patched and reverts.
    require_patched_modules
    with_stock_revert run taskset -c "${CORE_PIN_SMOKE}" \
        cargo test -p vmm-backend --all-features --test contract_kvm \
        -- --ignored --nocapture --test-threads=1
}

gate_backend_preemption() {
    require_patched_modules
    with_stock_revert run taskset -c "${CORE_PIN_MEASURE}" timeout 120 \
        cargo test -p vmm-backend --test live_preemption \
        -- --ignored --nocapture --test-threads=1
}

gate_linux_boot() {
    require_stock_modules
    run taskset -c "${CORE_PIN_SMOKE}" timeout 180 \
        cargo test -p vmm-core --test live_linux_boot \
        -- --ignored --nocapture --test-threads=1
}

gate_m1_m2() {
    require_stock_modules
    run taskset -c "${CORE_PIN_SMOKE}" \
        cargo test -p vmm-core --test live_m1_m2 -- --ignored --test-threads=1
}

gate_determinism() {
    require_patched_modules
    with_stock_revert run taskset -c "${CORE_PIN_MEASURE}" \
        cargo test -p vmm-core --test live_determinism -- --ignored --test-threads=1
}

gate_core_preemption() {
    require_patched_modules
    with_stock_revert run taskset -c "${CORE_PIN_MEASURE}" timeout 150 \
        cargo test -p vmm-core --test live_preemption \
        -- --ignored --nocapture --test-threads=1
}

gate_snapshot_branch() {
    require_patched_modules
    with_stock_revert run taskset -c "${CORE_PIN_FRONTIER}" \
        cargo test -p vmm-core --test live_snapshot_branch -- --ignored --test-threads=1
}

# --- pvclock: g0 smoke FIRST, then the full gate. See the ordering note. -----

gate_pvclock_g0() {
    require_patched_modules
    with_stock_revert run taskset -c "${CORE_PIN_MEASURE}" \
        cargo test -p vmm-core --release --test live_pvclock \
        -- --ignored --test-threads=1 g0_smoke_boot_registers_and_reads_sane_time
}

gate_pvclock() {
    require_patched_modules
    with_stock_revert run taskset -c "${CORE_PIN_MEASURE}" \
        cargo test -p vmm-core --release --test live_pvclock -- --ignored --test-threads=1
}

gate_host_plane() {
    require_patched_modules
    with_stock_revert run taskset -c "${CORE_PIN_MEASURE}" \
        cargo test -p vmm-core --release --test live_host_plane -- --ignored --nocapture
}

gate_moment_address() {
    require_patched_modules
    with_stock_revert run taskset -c "${CORE_PIN_MEASURE}" \
        cargo test -p vmm-core --release --test live_moment_address -- --ignored --nocapture
}

gate_sdk() {
    require_patched_modules
    with_stock_revert run taskset -c "${CORE_PIN_MEASURE}" \
        cargo test -p vmm-core --release --test live_sdk -- --ignored --nocapture
}

gate_dirty_remap() {
    require_patched_modules
    with_stock_revert run taskset -c "${CORE_PIN_MEASURE}" timeout 7200 \
        cargo test -p vmm-core --release --test live_dirty_remap \
        -- --ignored --nocapture --test-threads=1
}

gate_nonquiescent() {
    require_patched_modules
    with_stock_revert run taskset -c "${CORE_PIN_FRONTIER}" timeout 3600 \
        cargo test -p vmm-core --test live_nonquiescent_snapshot \
        -- --ignored --nocapture --test-threads=1
}

gate_branching_demo() {
    require_patched_modules
    with_stock_revert run taskset -c "${CORE_PIN_FRONTIER}" timeout 3600 \
        cargo test -p vmm-core --test live_branching_demo \
        -- --ignored --nocapture --test-threads=1
}

# Rung 5, the acceptance matrix's hardware cells. BOTH entry points run: the
# existing box_corpus gate (untouched — still the reference) and the packaged
# `acceptance-suite --host det-cfl-v1` path. Running both side by side is how
# the hardware parity run that gates the live_* migration gets its evidence.
gate_corpus_body() {
    run taskset -c "${CORE_PIN_MEASURE}" \
        cargo test -p vmm-core --test box_corpus -- --ignored --nocapture
    run taskset -c "${CORE_PIN_MEASURE}" \
        cargo run --release -p acceptance-suite --features real-vmm -- \
        run --manifest docs/corpus-manifest.toml --host det-cfl-v1
}

gate_corpus() {
    require_patched_modules
    with_stock_revert gate_corpus_body
}

gate_postgres() {
    require_patched_modules
    with_stock_revert run taskset -c "${CORE_PIN_MEASURE}" timeout 1500 \
        cargo test -p vmm-core --test live_postgres \
        -- --ignored --nocapture --test-threads=1 p2_postgres_deterministic_twice_patched
}

gate_seal_rate() {
    require_patched_modules
    with_stock_revert run taskset -c "${CORE_PIN_MEASURE}" timeout 7200 \
        cargo test -p vmm-core --test seal_rate_sweep \
        -- --ignored --nocapture --test-threads=1
}

gate_materialization() {
    require_patched_modules
    with_stock_revert run taskset -c "${CORE_PIN_MEASURE}" timeout 7200 \
        cargo test -p campaign-runner --test live_materialization \
        -- --ignored --nocapture --test-threads=1
}

gate_harmony_bridge() {
    require_patched_modules
    with_stock_revert run taskset -c "${CORE_PIN_FRONTIER}" \
        cargo test --release -p campaign-runner --test live_harmony_bridge \
        -- --ignored --nocapture --test-threads=1
}

gate_film() {
    require_patched_modules
    with_stock_revert run taskset -c "${CORE_PIN_FRONTIER}" timeout 7200 \
        cargo test -p campaign-runner --test live_film \
        -- --ignored --nocapture --test-threads=1
}

gate_exec_improvisation() {
    require_patched_modules
    with_stock_revert run env INITRAMFS=initramfs-exec.cpio.gz \
        taskset -c "${CORE_PIN_MEASURE}" \
        cargo test -p vmm-core --release --test live_exec_improvisation \
        -- --ignored --nocapture
}

gate_postgres_docker() {
    require_patched_modules
    with_stock_revert run taskset -c "${CORE_PIN_MEASURE}" timeout 3000 \
        cargo test -p vmm-core --test live_postgres_docker \
        -- --ignored --nocapture --test-threads=1 p2_docker_postgres_deterministic_twice_patched
}

gate_runc_postgres() {
    require_patched_modules
    with_stock_revert run taskset -c "${CORE_PIN_FRONTIER}" timeout 4200 \
        cargo test -p vmm-core --test live_runc_postgres \
        -- --ignored --nocapture --test-threads=1 r2_runc_postgres_deterministic_twice_patched
}

gate_k3s_postgres() {
    require_patched_modules
    with_stock_revert run taskset -c "${CORE_PIN_MEASURE}" timeout 14400 \
        cargo test -p vmm-core --test live_k3s_postgres \
        -- --ignored --nocapture --test-threads=1 k2_k3s_postgres_deterministic_twice_patched
}

gate_draw_probe_pair() {
    require_patched_modules
    with_stock_revert run taskset -c "${CORE_PIN_FRONTIER}" \
        cargo test --release -p campaign-runner --test live_draw_probe_pair \
        -- --ignored --nocapture --test-threads=1
}

gate_draw_probe_diagnosis() {
    require_patched_modules
    with_stock_revert run taskset -c "${CORE_PIN_FRONTIER}" \
        cargo test --release -p campaign-runner --test live_draw_probe_diagnosis \
        -- --ignored --nocapture --test-threads=1
}

# name|rung|description|function — the execution order is this array's order.
GATES=(
    "kvm-smoke|2|live KvmBackend smoke (stock)|gate_kvm_smoke"
    "contract|2|the Backend contract exam over the live backends|gate_contract"
    "backend-preemption|2|run_until exactness on real PMU hardware|gate_backend_preemption"
    "linux-boot|3|boot a real Linux guest (stock)|gate_linux_boot"
    "m1-m2|3|the M1/M2 payload identity tests|gate_m1_m2"
    "determinism|3|the core identity suite|gate_determinism"
    "core-preemption|3|preemption identity|gate_core_preemption"
    "snapshot-branch|3|snapshot + branch identity|gate_snapshot_branch"
    "pvclock-g0|3|pvclock g0 SMOKE — must pass before pvclock|gate_pvclock_g0"
    "pvclock|3|the full paravirt-clock gate (G1/G2/G3 + perf)|gate_pvclock"
    "host-plane|3|host-plane fault at a Moment|gate_host_plane"
    "moment-address|3|the (timeline, Moment) address|gate_moment_address"
    "sdk|3|the cooperating-guest SDK channel|gate_sdk"
    "dirty-remap|3|dirty-log derive + remap restore|gate_dirty_remap"
    "nonquiescent|3|non-quiescent snapshot|gate_nonquiescent"
    "branching-demo|3|the single-node branching demo|gate_branching_demo"
    "exec-improvisation|4|Exec taint on a live timeline|gate_exec_improvisation"
    "corpus|5|the C1 acceptance cells, both entry points|gate_corpus"
    "postgres|5|bare-Postgres workload identity|gate_postgres"
    "seal-rate|5|the seal-rate sweep|gate_seal_rate"
    "materialization|5|lazy materialization|gate_materialization"
    "harmony-bridge|5|the campaign bridge|gate_harmony_bridge"
    "draw-probe-pair|5|the game workload's draw probe|gate_draw_probe_pair"
    "draw-probe-diagnosis|5|draw-probe diagnostic|gate_draw_probe_diagnosis"
    "film|5|film the game workload|gate_film"
    # The multi-hour container/orchestrator workloads. LAST by design: they are
    # the most expensive cells in the matrix, and a window that cannot afford
    # them should still have run everything above. `--skip` them explicitly when
    # the lease is short.
    "postgres-docker|5|Postgres in a Docker image|gate_postgres_docker"
    "runc-postgres|5|Postgres under runc|gate_runc_postgres"
    "k3s-postgres|5|Postgres under k3s (the longest gate)|gate_k3s_postgres"
)

gate_name() { echo "${1%%|*}"; }
gate_rung() { echo "$1" | cut -d'|' -f2; }
gate_desc() { echo "$1" | cut -d'|' -f3; }
gate_fn() { echo "${1##*|}"; }

list_gates() {
    printf '%-20s %-5s %s\n' "GATE" "RUNG" "WHAT IT PROVES"
    local g
    for g in "${GATES[@]}"; do
        printf '%-20s %-5s %s\n' "$(gate_name "$g")" "$(gate_rung "$g")" "$(gate_desc "$g")"
    done
}

# ---------------------------------------------------------------------------
# Argument handling: named gates to run, or --skip to exclude.
# ---------------------------------------------------------------------------
selected=()
skipped=()
mode="all"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --list) list_gates; exit 0 ;;
        --skip)
            shift
            [[ $# -gt 0 ]] || die "--skip needs at least one gate name (see --list)"
            while [[ $# -gt 0 && "$1" != --* ]]; do
                skipped+=("$1")
                shift
            done
            ;;
        -h|--help)
            sed -n '3,42p' "${BASH_SOURCE[0]}"
            exit 0
            ;;
        --*) die "unknown option '$1' (see --help)" ;;
        *)
            mode="named"
            selected+=("$1")
            shift
            ;;
    esac
done

contains() {
    local needle="$1"; shift
    local x
    for x in "$@"; do [[ "$x" == "${needle}" ]] && return 0; done
    return 1
}

# A named gate that does not exist is a typo, not a request to run nothing.
if [[ "${mode}" == "named" ]]; then
    for want in "${selected[@]}"; do
        found=0
        for g in "${GATES[@]}"; do
            [[ "$(gate_name "$g")" == "${want}" ]] && found=1
        done
        (( found )) || die "unknown gate '${want}' (see --list)"
    done
fi
for want in "${skipped[@]:-}"; do
    [[ -z "${want}" ]] && continue
    found=0
    for g in "${GATES[@]}"; do
        [[ "$(gate_name "$g")" == "${want}" ]] && found=1
    done
    (( found )) || die "unknown gate '${want}' in --skip (see --list)"
done

# ---------------------------------------------------------------------------
# Run.
# ---------------------------------------------------------------------------
require_host_baseline

ran=()
failed=()
for g in "${GATES[@]}"; do
    name="$(gate_name "$g")"
    if [[ "${mode}" == "named" ]] && ! contains "${name}" "${selected[@]}"; then
        continue
    fi
    if contains "${name}" "${skipped[@]:-}"; then
        note "SKIP ${name} (requested)"
        continue
    fi
    note "=== gate ${name} (rung $(gate_rung "$g")): $(gate_desc "$g")"
    if "$(gate_fn "$g")"; then
        ran+=("${name}")
        note "=== gate ${name}: PASS"
    else
        failed+=("${name}")
        note "=== gate ${name}: FAIL"
    fi
done

# A run that executed nothing is a failure, not a success. This is the same
# rule the acceptance runner applies to an empty cell selection.
if [[ ${#ran[@]} -eq 0 && ${#failed[@]} -eq 0 ]]; then
    die "no gate ran: every gate was filtered out. A run that executes nothing is not a pass."
fi

note "----------------------------------------------------------------"
note "ran:    ${ran[*]:-<none>}"
note "failed: ${failed[*]:-<none>}"
if [[ ${#failed[@]} -gt 0 ]]; then
    exit 1
fi
note "all selected gates passed"
