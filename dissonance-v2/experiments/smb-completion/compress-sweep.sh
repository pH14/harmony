#!/bin/bash
# Compress every derived archive and replay artifact except the two newest
# archives. Replay audits are dropped from the chain, so nothing waits on a
# verdict any more. Archives regenerate byte-exact by replaying their run
# stream, and each file's sha256 and byte count are recorded before compression.
set -uo pipefail
root=/root/harmony-smb-goal/dissonance-v2/target/smb-completion
keep1="${1:?usage: compress-sweep.sh <keep-archive-1> <keep-archive-2>}"
keep2="${2:?missing second protected archive}"
files=0
freed=0

compress_one() {
    local path="$1"
    [ -f "$path" ] || return 0
    local dir name size sha
    dir=$(dirname "$path"); name=$(basename "$path")
    size=$(stat -c %s "$path")
    sha=$(sha256sum "$path" | cut -d' ' -f1)
    printf '{"file":"%s","sha256":"%s","bytes":%s,"compressed":"%s.zst"}\n' \
        "$name" "$sha" "$size" "$name" > "$dir/$name.manifest.json"
    if nice -n 15 zstd -T3 -q --rm "$path"; then
        local after; after=$(stat -c %s "$path.zst")
        files=$((files+1)); freed=$((freed+size-after))
        echo "OK $(basename "$dir")/$name $((size/1000000))MB -> $((after/1000000))MB"
    else
        rm -f "$dir/$name.manifest.json"
        echo "FAILED $path"
    fi
}

for path in "$root"/*/archive-live.json; do
    [ -f "$path" ] || continue
    name=$(basename "$(dirname "$path")")
    if [ "$name" = "$keep1" ] || [ "$name" = "$keep2" ]; then
        echo "SKIP $name (protected)"
        continue
    fi
    compress_one "$path"
done

for path in "$root"/*/archive-replay.json "$root"/*/campaign-report-replay.json; do
    compress_one "$path"
done

echo "SWEEP_FILES=$files"
echo "SWEEP_FREED_GB=$((freed/1000000000))"
df -h /root | tail -1
echo "SWEEP_SENTINEL: DONE"
