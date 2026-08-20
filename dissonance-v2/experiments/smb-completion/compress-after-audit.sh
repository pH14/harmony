#!/bin/bash
# Compress what a green audit makes redundant.
#
# Usage: compress-after-audit.sh <audited-link> <origin-link> <newest-link-number>
#
# When link N's audit verifies, two things become derived-and-verified: N's own
# replay artifacts, which restate what the live run already recorded, and the
# origin archive N consumed, which that audit has now exercised end to end.
# Both regenerate byte-exact by replay. The two newest archives are never
# touched because they are the running link's origin and its predecessor.
set -uo pipefail
root=/root/harmony-smb-goal/dissonance-v2/target/smb-completion
audited="${1:?usage: compress-after-audit.sh <audited-link> <origin-link> <newest-number>}"
origin="${2:?missing origin link}"
newest="${3:?missing newest link number}"

verdict="$root/$audited-conquest/replay-verdict.json"
if ! grep -q '"replay_verified": true' "$verdict" 2>/dev/null; then
    echo "REFUSING: $audited has no green verdict at $verdict"
    exit 2
fi

compress_one() {
    local path="$1"
    [ -f "$path" ] || { echo "skip (absent) $path"; return 0; }
    local dir name size sha
    dir=$(dirname "$path"); name=$(basename "$path")
    size=$(stat -c %s "$path")
    sha=$(sha256sum "$path" | cut -d' ' -f1)
    printf '{"file":"%s","sha256":"%s","bytes":%s,"compressed":"%s.zst"}\n' \
        "$name" "$sha" "$size" "$name" > "$dir/$name.manifest.json"
    if nice -n 15 zstd -T3 -q --rm "$path"; then
        echo "OK $(basename "$dir")/$name $((size/1000000))MB -> $(( $(stat -c %s "$path.zst") /1000000 ))MB"
    else
        rm -f "$dir/$name.manifest.json"
        echo "FAILED $path"
    fi
}

compress_one "$root/$audited-conquest/archive-replay.json"
compress_one "$root/$audited-conquest/campaign-report-replay.json"

# The origin archive is skipped when it is one of the two newest.
origin_num=${origin#c}
if [ "$origin_num" -ge "$((newest-1))" ]; then
    echo "SKIP origin $origin: within the two newest archives"
else
    compress_one "$root/$origin-conquest/archive-live.json"
fi

df -h /root | tail -1
echo "AUDIT_COMPRESS_SENTINEL: DONE"
