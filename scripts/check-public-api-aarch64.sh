#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# The aarch64-linux leg of the frozen public-API gate (quality-d).
#
# The in-tree `tests/public_api.rs` guards freeze the **x86-64 Linux** surface:
# `vmm-backend` and `vmm-core` gate part of their public surface on
# `target_arch`, and their guard tests return early on any other target. So the
# aarch64-linux-only surface — `vmm_backend::LiveKvm` (including a
# `pub unsafe fn`) and `vmm_core::vendor::arm64::bringup::{boot_selected,
# boot_selected_control}` — was frozen nowhere. This script is that leg.
#
# It runs on an x86-64 runner and CROSS-TARGETS the generator, the mirror image
# of the macOS recipe in docs/CODE-QUALITY.md ("Public-API snapshots"). No
# aarch64 artifact is ever linked or run, so no aarch64 C toolchain is needed —
# same standing as the "cross-check aarch64" clippy lane in quality.yml.
#
# NON-VACUITY IS THE POINT (AGENTS.md review rule 4, "Gate vacuity"). The x86
# guards skip loudly when the nightly or `cargo-public-api` is absent, so that a
# stable-only `cargo nextest` stays green — which is also how the public-api job
# once reported green-by-skipping rather than green-by-checking. This script has
# no skip branch at all: a missing toolchain, a missing target, a missing tool
# or a missing snapshot is a hard failure, and every crate prints the snapshot it
# was compared against, so the job's own log shows the comparison happening.
#
# And the set of crates it must compare is DISCOVERED FROM THE TREE, never taken
# on the crate list's word — see "Coverage" below. A gate that checks its own
# work against its own list can be silenced by shrinking the list; this one
# cannot.
#
# Usage:
#   scripts/check-public-api-aarch64.sh            # check (CI, and locally)
#   UPDATE_PUBLIC_API=1 scripts/check-public-api-aarch64.sh   # refresh goldens
#
# Refresh after an intentional, reviewed API change, then review the diff. A
# change to an arch-neutral item moves BOTH the x86 snapshot (via
# `UPDATE_PUBLIC_API=1 cargo test -p <crate> --test public_api`) and, for the two
# arch-divergent crates, the aarch64 snapshot this script maintains.
set -euo pipefail

# Keep in sync with each `tests/public_api.rs` (`PINNED_NIGHTLY`), the
# `public-api` / `public-api-aarch64` jobs in .github/workflows/quality.yml, and
# docs/CODE-QUALITY.md. `cargo public-api` needs rustdoc-JSON, which is
# nightly-only; pinning keeps the output reproducible.
PINNED_NIGHTLY="${PINNED_NIGHTLY:-nightly-2026-06-16}"
TARGET=aarch64-unknown-linux-gnu

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "${repo_root}"

# Every crate carrying a frozen public-API snapshot, in the same order as the
# `-p` list of the `public-api` job. The aarch64 leg covers ALL of them, not
# just the two that currently diverge: a crate that grows its first
# aarch64-only `pub` item must fail here rather than drift in silently.
#
# This list carries the cargo SELECTOR (in-workspace `-p` vs the out-of-workspace
# `--manifest-path`), which is why it is written out rather than derived. It is
# NOT trusted to define coverage: the "Coverage" block below reconciles it
# against the snapshots actually present in the tree, in both directions, and
# fails if the two disagree.
#
#   <snapshot dir>|<cargo selector>
CRATES=(
    "consonance/hypercall-proto|-p hypercall-proto"
    "consonance/hypercall-doorbell|-p hypercall-doorbell"
    "consonance/snapshot-store|-p snapshot-store"
    "consonance/unison|-p unison"
    "consonance/vtime|-p vtime"
    "consonance/vm-state|-p vm-state"
    "consonance/vmm-backend|-p vmm-backend"
    "consonance/vmm-core|-p vmm-core"
    "consonance/lapic|-p lapic"
    "consonance/gicv3|-p gicv3"
    "consonance/telemetry|-p telemetry"
    "consonance/environment|-p environment"
    "consonance/control-proto|-p control-proto"
    "consonance/acceptance-suite|-p acceptance-suite"
    # Out-of-workspace (its own manifest), exactly as in the x86 job.
    "consonance/harmony-linux/sdk|--manifest-path consonance/harmony-linux/sdk/Cargo.toml"
)

die() { echo "FAIL: $*" >&2; exit 1; }

# --- Preflight. Every one of these is a hard failure, never a skip. ----------
command -v cargo >/dev/null 2>&1 || die "cargo not found"
command -v rustup >/dev/null 2>&1 || die "rustup not found (needed to select ${PINNED_NIGHTLY} and check the ${TARGET} std)"

cargo "+${PINNED_NIGHTLY}" --version >/dev/null 2>&1 ||
    die "pinned toolchain ${PINNED_NIGHTLY} is not installed
     rustup toolchain install ${PINNED_NIGHTLY}"

cargo "+${PINNED_NIGHTLY}" public-api --version >/dev/null 2>&1 ||
    die "cargo-public-api is not installed for ${PINNED_NIGHTLY}
     cargo install --locked cargo-public-api    (scripts/install-quality-tools.sh)"

rustup target list --installed --toolchain "${PINNED_NIGHTLY}" 2>/dev/null |
    grep -qx "${TARGET}" ||
    die "target ${TARGET} is not installed for ${PINNED_NIGHTLY}
     rustup target add ${TARGET} --toolchain ${PINNED_NIGHTLY}"

echo "public-api aarch64 leg: target ${TARGET}, toolchain ${PINNED_NIGHTLY}, $(cargo "+${PINNED_NIGHTLY}" public-api --version)"

# --- Coverage. Discovered from the tree, never asserted against CRATES. ------
#
# CRATES above is hand-maintained and duplicated from the x86 `public-api` job's
# `-p` list, so it can go stale — that set has already grown once, 2 crates to
# 15. Checking the loop's work against `${#CRATES[@]}` would not notice, because
# emptying or truncating the list shrinks BOTH sides of that comparison at once:
# an empty list would compare nothing and report "0/0 ... no drift", and a crate
# that gains a snapshot without gaining an entry here would have its aarch64
# surface frozen nowhere, with nothing anywhere saying so.
#
# So the expected set comes from the tree instead. The committed snapshots are
# the source of truth: a crate carrying `tests/public-api*.txt` is a crate whose
# public surface this project freezes, and this leg owes it a comparison. Three
# reconciliations, none of which a shrinking CRATES can satisfy:
#
#   (a) every discovered snapshot must be covered by CRATES  — catches a stale
#       list, which is the realistic failure: crate 16 lands, nobody edits here;
#   (b) every CRATES entry must have a discovered snapshot   — catches the
#       reverse, an entry left behind by a crate whose golden was removed;
#   (c) the final compared-count is checked against the DISCOVERED total.
#
# Discovery deliberately walks the working tree rather than `git ls-files`: an
# uncommitted golden is still a golden this leg must not silently ignore.
# `target/` (a restored build cache) and `.git` are pruned; nothing under either
# is a frozen contract.
discovered=()
discovered_dirs=" "
while IFS= read -r snapshot_path; do
    [ -n "${snapshot_path}" ] || continue
    case "${snapshot_path}" in
        */tests/public-api*.txt) ;;
        *) die "public-API snapshot in an unexpected place: ${snapshot_path}
     this leg discovers goldens at <crate-dir>/tests/public-api*.txt. Teach the
     discovery about this one rather than leaving its surface unfrozen." ;;
    esac
    dir=${snapshot_path%/tests/public-api*.txt}
    # vmm-backend and vmm-core each carry two goldens (x86 + aarch64); one crate.
    case "${discovered_dirs}" in
        *" ${dir} "*) continue ;;
    esac
    discovered+=("${dir}")
    discovered_dirs="${discovered_dirs}${dir} "
done < <(find . \( -name target -o -name .git \) -prune -o \
             -name 'public-api*.txt' -print | sed 's|^\./||' | sort)

discovered_count=${#discovered[@]}
[ "${discovered_count}" -gt 0 ] ||
    die "discovered no tests/public-api*.txt anywhere under ${repo_root}
     Either coverage discovery is broken or the frozen-API goldens are gone.
     Either way this gate has nothing to check and must not report success."

covered_dirs=" "
for entry in "${CRATES[@]}"; do
    covered_dirs="${covered_dirs}${entry%%|*} "
done

uncovered=()
for dir in "${discovered[@]}"; do
    case "${covered_dirs}" in
        *" ${dir} "*) ;;
        *) uncovered+=("${dir}") ;;
    esac
done
if [ ${#uncovered[@]} -ne 0 ]; then
    for dir in "${uncovered[@]}"; do
        echo "  UNCOVERED ${dir}: freezes a public API that this leg never compares" >&2
    done
    die "${#uncovered[@]} crate(s) carry a frozen public-API snapshot the aarch64 leg does not cover: ${uncovered[*]}
     Their aarch64 surface is frozen NOWHERE, which is the drift this gate
     exists to catch. Add each to CRATES above with its cargo selector (and to
     the \`public-api\` job's -p list in .github/workflows/quality.yml if it is
     missing there too)."
fi

stale=()
for entry in "${CRATES[@]}"; do
    dir=${entry%%|*}
    case "${discovered_dirs}" in
        *" ${dir} "*) ;;
        *) stale+=("${dir}") ;;
    esac
done
if [ ${#stale[@]} -ne 0 ]; then
    die "${#stale[@]} CRATES entry(ies) name a crate with no tests/public-api*.txt: ${stale[*]}
     The list is stale. Drop the entry if that crate's surface is deliberately
     no longer frozen; restore its golden if it is not."
fi

echo "coverage: ${discovered_count} crate(s) in the tree carry a frozen public-API snapshot, and the crate list covers all ${discovered_count}"
echo

work=$(mktemp -d)
trap 'rm -rf "${work}"' EXIT

failed=()
updated=0
compared=0

for entry in "${CRATES[@]}"; do
    dir=${entry%%|*}
    selector=${entry#*|}
    name=$(basename "${dir}")

    # An arch-divergent crate carries its own aarch64 snapshot. An arch-neutral
    # one is compared against the SAME snapshot the x86 leg freezes — which is
    # the assertion that it really is arch-neutral, and is what makes a newly
    # aarch64-only `pub` item anywhere in the workspace fail this gate.
    arch_snapshot="${dir}/tests/public-api-aarch64.txt"
    if [ -f "${arch_snapshot}" ]; then
        snapshot="${arch_snapshot}"
        kind="aarch64-specific"
    else
        snapshot="${dir}/tests/public-api.txt"
        kind="shared with x86 (arch-neutral)"
    fi
    [ -f "${snapshot}" ] || die "${name}: no snapshot at ${snapshot}"

    actual="${work}/${name}.txt"
    # shellcheck disable=SC2086 # selector is a deliberate two-word argument
    if ! cargo "+${PINNED_NIGHTLY}" public-api ${selector} --all-features -sss \
            --color never --target "${TARGET}" > "${actual}" 2> "${work}/${name}.err"; then
        echo "  ${name}: GENERATION FAILED" >&2
        sed 's/^/      /' "${work}/${name}.err" >&2
        failed+=("${name} (generation)")
        continue
    fi

    if [ -n "${UPDATE_PUBLIC_API:-}" ]; then
        # Only ever write a dedicated aarch64 snapshot. Refusing to overwrite a
        # shared x86 snapshot from this script keeps the x86 leg's goldens the
        # x86 leg's business, and makes "this crate just diverged" an explicit
        # act: the diff below tells the author to create the file.
        if [ "${snapshot}" = "${arch_snapshot}" ]; then
            cp "${actual}" "${arch_snapshot}"
            echo "  ${name}: updated ${arch_snapshot}"
            updated=$((updated + 1))
        elif diff -q "${snapshot}" "${actual}" >/dev/null; then
            echo "  ${name}: unchanged, still arch-neutral (no aarch64 snapshot needed)"
        else
            cp "${actual}" "${arch_snapshot}"
            echo "  ${name}: NEWLY ARCH-DIVERGENT — created ${arch_snapshot}"
            updated=$((updated + 1))
        fi
        continue
    fi

    if diff -u "${snapshot}" "${actual}" > "${work}/${name}.diff"; then
        # The positive statement the gate is judged on: this crate's aarch64
        # surface was regenerated and compared, and it matched.
        echo "  COMPARED ${name}: aarch64 surface matches ${snapshot} [${kind}]"
        compared=$((compared + 1))
    else
        echo
        echo "  DRIFT ${name}: aarch64 public API does not match ${snapshot}" >&2
        sed 's/^/      /' "${work}/${name}.diff" >&2
        if [ "${snapshot}" = "${arch_snapshot}" ]; then
            echo "      Intentional and reviewed? Refresh with:" >&2
            echo "          UPDATE_PUBLIC_API=1 scripts/check-public-api-aarch64.sh" >&2
        else
            echo "      ${name} was arch-neutral and is not any more: it now has a" >&2
            echo "      public surface that exists only on ${TARGET}. If that is" >&2
            echo "      intentional and reviewed, give it its own frozen snapshot:" >&2
            echo "          UPDATE_PUBLIC_API=1 scripts/check-public-api-aarch64.sh" >&2
            echo "      which will create ${arch_snapshot}." >&2
        fi
        echo >&2
        failed+=("${name}")
    fi
done

echo
if [ -n "${UPDATE_PUBLIC_API:-}" ]; then
    echo "public-api aarch64 leg: ${updated} snapshot(s) updated; review the diff before committing"
    [ ${#failed[@]} -eq 0 ] || die "${#failed[@]} crate(s) failed to generate: ${failed[*]}"
    exit 0
fi

if [ ${#failed[@]} -ne 0 ]; then
    die "${#failed[@]} crate(s) failed the aarch64 public-API gate: ${failed[*]}"
fi

# Guard the gate against itself: a silently-emptied crate list, or a loop that
# never executed, must not report success. The expected count is the number of
# snapshot crates DISCOVERED IN THE TREE, not `${#CRATES[@]}` — comparing the
# loop's work against the list that drove it is self-referential and both sides
# shrink together.
[ "${compared}" -eq "${discovered_count}" ] ||
    die "expected ${discovered_count} crates compared, one per snapshot discovered in the tree, got ${compared} — the gate did not run in full"

echo "public-api aarch64 leg: ${compared}/${discovered_count} crates compared against the frozen aarch64 surface, no drift"
