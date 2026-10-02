# SPDX-License-Identifier: AGPL-3.0-or-later
# shellcheck shell=bash disable=SC2034
# Guest backend arguments shared by historical-search.sh and
# historical-replay.sh. BACKEND=consonance boots guest/bzImage under KVM.
# BACKEND=uml runs the User-mode Linux profile in guest/uml as an ordinary
# user, under the profile qualifier's filter that denies ptrace and every KVM
# ioctl, and records the credentials and denial beside the report.

historical_backend() {
    local denial=$1
    launcher=()
    case "${BACKEND:-consonance}" in
        consonance)
            test -s "${PWD}/guest/bzImage"
            guest_arguments=(--backend consonance --kernel "${PWD}/guest/bzImage")
            ;;
        uml)
            local profile=${PWD}/guest/uml
            chmod +x "${profile}/linux" "${profile}/harmony-uml-qualify"
            test -s "${profile}/profile.json"
            guest_arguments=(--backend uml --uml-profile "${profile}")
            launcher=("${profile}/harmony-uml-qualify" exec --report "${denial}" --)
            ;;
        *)
            echo "historical: unknown BACKEND ${BACKEND}" >&2
            return 2
            ;;
    esac
}

historical_snapshots() {
    local summary=$1
    [[ "${BACKEND:-consonance}" == uml && -s "${summary}" ]] || {
        echo "not counted"
        return
    }
    jq -r '.telemetry.target // {} |
        "captures \(.["vmm.uml.capture.count"] // 0), restores \(.["vmm.uml.restore.count"] // 0), fresh restores \(.["vmm.uml.fresh_restore.count"] // 0), imports \(.["vmm.uml.import.count"] // 0)"' \
        "${summary}"
}

historical_snapshots_used() {
    [[ "${BACKEND:-consonance}" == uml ]] || return 0
    [[ "$1" =~ captures\ ([0-9]+),\ restores\ ([0-9]+),\ fresh\ restores\ ([0-9]+) ]] || return 1
    (( BASH_REMATCH[1] > 0 && BASH_REMATCH[2] + BASH_REMATCH[3] > 0 ))
}
