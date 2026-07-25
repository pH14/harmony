#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Hermetic negative-control + property suite for box-window.sh (tasks/164, hm-nvwx).
#
# Runs entirely off-box on macOS or Linux: the KVM module and its tools
# (lsmod/rmmod/insmod/modprobe) are faked by a PATH shim backed by a KVMSTATE
# file, and lease/lock bookkeeping is relocated into a sandbox via box-window.sh's
# BOX_WINDOW_* hooks. This exercises the *decision logic* — when the window opens,
# when it reverts, which lease keeps it open — not the real module transition (the
# real load/revert is proved on the box; see the PR body).
#
# THE DEFINING REGRESSION (spec §"The regression that defines 'fixed'"): acquire a
# window, let the acquiring ssh shell die (modelled by pinning the lease's recorded
# pid to a guaranteed-dead value — exactly the "deadline pid core" a short-lived
# `ssh <box> 'box-window.sh acquire x'` leaves behind), then have a well-behaved
# concurrent lane invoke a verb, and assert the patched module is NOT reverted.
# Scenario A runs it against the OLD committed script and asserts it DOES revert
# (the control must fail on old code); Scenario B runs the identical sequence
# against the working-tree script and asserts it does NOT. A fix whose test never
# failed on the old code proves nothing.
#
# Usage:  bash scripts/box-window-test.sh
# The old script is read from git (default: the pre-fix commit) so the control keeps
# failing on old code even after this fix lands; override with BOX_WINDOW_OLD_REF.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
NEW="$HERE/box-window.sh"
OLD_REF="${BOX_WINDOW_OLD_REF:-c48d0901}"

SANDBOX="$(mktemp -d "${TMPDIR:-/tmp}/box-window-test.XXXXXX")"
trap 'rm -rf "$SANDBOX"' EXIT

STOCK=1396736
PATCHED=1400832
# A pid at/above every mainstream pid_max (Linux 4194304, macOS ~99998) never
# names a live process, so kill -0 on it is deterministically ESRCH: a portable
# stand-in for "the ssh shell that took this lease has exited."
DEAD=4194304
KVMSTATE="$SANDBOX/kvmstate"

# --- fake module + tools (PATH shim) ----------------------------------------
FAKEBIN="$SANDBOX/bin"
mkdir -p "$FAKEBIN"
cat > "$FAKEBIN/lsmod" <<EOF
#!/usr/bin/env bash
sz=\$(cat "$KVMSTATE" 2>/dev/null || echo $STOCK)
echo "Module                  Size  Used by"
echo "kvm_intel             417792  0"
echo "kvm                    \$sz  1 kvm_intel"
EOF
# insmod loads the patched module; modprobe (re)loads stock; rmmod is a no-op
# (lsmod size is only re-read after a subsequent load, matching real behaviour).
printf '#!/usr/bin/env bash\necho %s > "%s"\n' "$PATCHED" "$KVMSTATE" > "$FAKEBIN/insmod"
printf '#!/usr/bin/env bash\necho %s > "%s"\n' "$STOCK"   "$KVMSTATE" > "$FAKEBIN/modprobe"
printf '#!/usr/bin/env bash\nexit 0\n' > "$FAKEBIN/rmmod"
chmod +x "$FAKEBIN"/*
export PATH="$FAKEBIN:$PATH"

# --- old script, with its hard-coded /root paths redirected into the sandbox -
OLD="$SANDBOX/box-window-old.sh"
if ! git -C "$HERE" show "$OLD_REF:scripts/box-window.sh" > "$OLD.orig" 2>/dev/null; then
    echo "FATAL: cannot read old script from $OLD_REF:scripts/box-window.sh" >&2
    exit 3
fi
sed -e "s|^LOCK=/root/box-window.lock|LOCK=$SANDBOX/old.lock|" \
    -e "s|^LEASES=/root/box-window-leases|LEASES=$SANDBOX/old-leases|" \
    -e "s|^B=/root/.*|B=$SANDBOX/ko|" \
    "$OLD.orig" > "$OLD"
mkdir -p "$SANDBOX/ko"; : > "$SANDBOX/ko/kvm.ko"; : > "$SANDBOX/ko/kvm-intel.ko"

# --- harness plumbing -------------------------------------------------------
PASS=0; FAIL=0
ok()   { PASS=$((PASS+1)); printf '  ok   %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  FAIL %s\n' "$1"; }
check(){ if [ "$2" = "$3" ]; then ok "$1 ($2)"; else bad "$1 (want $3, got $2)"; fi; }

kvm()         { cat "$KVMSTATE"; }
set_stock()   { echo "$STOCK" > "$KVMSTATE"; }
reset_new()   { rm -rf "$SANDBOX/new-leases" "$SANDBOX/new.lock"; set_stock; }
reset_old()   { rm -rf "$SANDBOX/old-leases" "$SANDBOX/old.lock"; set_stock; }
lease_count() { find "$SANDBOX/$1-leases" -maxdepth 1 -type f ! -name '.exclusive' 2>/dev/null | wc -l | tr -d ' '; }

# NEW script driver. Leading KEY=VAL tokens become environment overrides, e.g.
#   nw BOX_WINDOW_NOW=1000 BOX_WINDOW_PID=- acquire crashed --ttl 10
new_env=(BOX_WINDOW_LEASES="$SANDBOX/new-leases" BOX_WINDOW_LOCK="$SANDBOX/new.lock"
         BOX_WINDOW_KVM_B="$SANDBOX/ko" BOX_WINDOW_STOCK_SIZE="$STOCK")
nw() {
    local envs=()
    while [ "$#" -gt 0 ] && [[ "$1" == *=* ]]; do envs+=("$1"); shift; done
    env "${new_env[@]}" ${envs[@]+"${envs[@]}"} bash "$NEW" "$@"
}

# portable timeout (macOS has neither `timeout` nor `gtimeout` by default): run a
# command, and if it is still alive after N seconds, terminate it.
with_timeout() { # seconds cmd...
    local secs="$1"; shift
    "$@" & local p=$!
    ( sleep "$secs"; kill -TERM "$p" 2>/dev/null; sleep 1; kill -KILL "$p" 2>/dev/null ) & local w=$!
    wait "$p" 2>/dev/null; local rc=$?
    kill "$w" 2>/dev/null; wait "$w" 2>/dev/null
    return "$rc"
}

# model "the ssh shell that acquired has exited" by pinning the lease's pid dead
kill_shell_old() { local f="$SANDBOX/old-leases/$1" c; read -r _ c < "$f"; echo "$DEAD $c" > "$f"; }
kill_shell_new() { local f="$SANDBOX/new-leases/$1" d c; read -r d _ c < "$f"; echo "$d $DEAD $c" > "$f"; }

echo "== box-window.sh tasks/164 regression suite =="
echo "sandbox: $SANDBOX   old-ref: $OLD_REF"

# ============================================================================
# Scenario A — negative control on the OLD script: MUST revert (bug present)
# ============================================================================
echo "-- A: defining regression on OLD script (expect RED: module reverted) --"
reset_old
bash "$OLD" acquire laneA >/dev/null 2>&1           # real acquire: loads patched
check "A: old window opened (patched)" "$(kvm)" "$PATCHED"
kill_shell_old laneA                                # acquiring shell exits
bash "$OLD" release laneB >/dev/null 2>&1           # well-behaved concurrent lane releases ITS lease
check "A: OLD reverted out from under laneA (the bug)" "$(kvm)" "$STOCK"

# ============================================================================
# Scenario B — identical sequence on the NEW script: MUST NOT revert (fixed)
# ============================================================================
echo "-- B: same sequence on NEW script (expect GREEN: module survives) --"
reset_new
nw acquire laneA >/dev/null 2>&1
check "B: new window opened (patched)" "$(kvm)" "$PATCHED"
check "B: laneA lease exists"          "$(lease_count new)" "1"
kill_shell_new laneA                                # same: acquiring shell exits
nw release laneB >/dev/null 2>&1                    # same well-behaved concurrent verb
check "B: NEW did NOT revert (laneA still time-live)" "$(kvm)" "$PATCHED"
check "B: laneA lease still held"                     "$(lease_count new)" "1"
nw release laneA >/dev/null 2>&1                     # last live lease out -> revert + verify
check "B: last-lease-out reverted+verified"           "$(kvm)" "$STOCK"
check "B: leases empty after release"                 "$(lease_count new)" "0"

# ============================================================================
# Scenario C — a crashed/abandoned lease still self-heals at TTL expiry
# ============================================================================
echo "-- C: expired lease is swept and window self-heals --"
reset_new
nw BOX_WINDOW_NOW=1000 BOX_WINDOW_PID=- acquire crashed --ttl 10 >/dev/null 2>&1
check "C: window open" "$(kvm)" "$PATCHED"
nw BOX_WINDOW_NOW=1005 release other >/dev/null 2>&1   # before deadline: stays open
check "C: not reverted before deadline" "$(kvm)" "$PATCHED"
nw BOX_WINDOW_NOW=2000 release other >/dev/null 2>&1   # after deadline, no live pid: sweep+revert
check "C: reverted after TTL lapsed"    "$(kvm)" "$STOCK"
check "C: expired lease swept"          "$(lease_count new)" "0"

# ============================================================================
# Scenario D — a live pid EXTENDS a lease past its TTL (long-orchestrator compat)
# ============================================================================
echo "-- D: live pid keeps a lease alive past its TTL, death releases it --"
reset_new
sleep 300 & holder=$!                                  # stand-in long-lived orchestrator
nw BOX_WINDOW_NOW=1000 BOX_WINDOW_PID="$holder" acquire orch --ttl 10 >/dev/null 2>&1
check "D: window open" "$(kvm)" "$PATCHED"
nw BOX_WINDOW_NOW=2000 release other >/dev/null 2>&1   # past TTL, but pid alive -> still live
check "D: past-TTL lease kept alive by live pid" "$(kvm)" "$PATCHED"
kill "$holder" 2>/dev/null; wait "$holder" 2>/dev/null
nw BOX_WINDOW_NOW=2000 release other >/dev/null 2>&1   # pid dead AND expired -> swept, revert
check "D: reverted once pid died after TTL"      "$(kvm)" "$STOCK"

# ============================================================================
# Scenario E — --exclusive still excludes both directions
# ============================================================================
echo "-- E: exclusivity still holds --"
reset_new
nw acquire shared1 >/dev/null 2>&1
with_timeout 3 env "${new_env[@]}" bash "$NEW" acquire excl --exclusive >/dev/null 2>&1
check "E: exclusive blocked while a shared lease lives" "$(lease_count new)" "1"
nw release shared1 >/dev/null 2>&1
reset_new
nw acquire excl --exclusive >/dev/null 2>&1
check "E: exclusive opened window" "$(kvm)" "$PATCHED"
with_timeout 3 env "${new_env[@]}" bash "$NEW" acquire joiner >/dev/null 2>&1
check "E: joiner blocked by exclusive holder" "$(lease_count new)" "1"
nw release excl >/dev/null 2>&1
check "E: reverted after exclusive released" "$(kvm)" "$STOCK"

# ============================================================================
# Scenario F — concurrent gates on distinct cores (the feature) still works
# ============================================================================
echo "-- F: three concurrent leases get distinct cores, 4th blocks --"
reset_new
f1=$(nw acquire g1 2>/dev/null)
f2=$(nw acquire g2 2>/dev/null)
f3=$(nw acquire g3 2>/dev/null)
check "F: g1 core" "$f1" "2"
check "F: g2 core" "$f2" "1"
check "F: g3 core" "$f3" "3"
check "F: window opened once" "$(kvm)" "$PATCHED"
check "F: three live leases" "$(lease_count new)" "3"
with_timeout 3 env "${new_env[@]}" bash "$NEW" acquire g4 >/dev/null 2>&1
check "F: 4th acquire blocks (all cores held)" "$(lease_count new)" "3"
nw release g1 >/dev/null 2>&1; check "F: still patched after 1 of 3 released" "$(kvm)" "$PATCHED"
nw release g2 >/dev/null 2>&1; check "F: still patched after 2 of 3 released" "$(kvm)" "$PATCHED"
nw release g3 >/dev/null 2>&1; check "F: reverted after last released" "$(kvm)" "$STOCK"

# ============================================================================
# Scenario G — renew extends a lease; renewing an already-expired lease fails
# ============================================================================
echo "-- G: renew extends the deadline; expired renew is refused --"
reset_new
nw BOX_WINDOW_NOW=1000 BOX_WINDOW_PID=- acquire long --ttl 10 >/dev/null 2>&1
nw BOX_WINDOW_NOW=1005 renew long 100 >/dev/null 2>&1   # deadline 1005+100 = 1105
nw BOX_WINDOW_NOW=1050 release other >/dev/null 2>&1    # 1050 < 1105: renewed lease still live
check "G: renewed lease kept window open" "$(kvm)" "$PATCHED"
if nw BOX_WINDOW_NOW=2000 renew long 100 >/dev/null 2>&1; then
    bad "G: renew of expired lease should fail"
else
    ok "G: renew of expired lease refused"
fi
nw BOX_WINDOW_NOW=2000 release other >/dev/null 2>&1    # clean up: sweep the now-expired lease
check "G: window reverted after expiry" "$(kvm)" "$STOCK"

# ----------------------------------------------------------------------------
echo
echo "== $PASS passed, $FAIL failed =="
[ "$FAIL" -eq 0 ]
