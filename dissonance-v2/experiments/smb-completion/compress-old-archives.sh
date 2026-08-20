#!/bin/bash
# Compress derived archives for runs older than the entrance family.
#
# archive-live.json is derived: replaying the run stream regenerates it
# byte-exact. The manifest records its sha256 and byte size before compression
# so the identity pin survives independently of the file. Streams, reports and
# censuses are untouched.
set -uo pipefail
root=/root/harmony-smb-goal/dissonance-v2/target/smb-completion
threads=3
compressed=0
failed=0
freed=0
for archive in "$root"/*/archive-live.json; do
    [ -f "$archive" ] || continue
    dir=$(dirname "$archive")
    name=$(basename "$dir")
    case "$name" in
        c11[0-9]-conquest|c1[2-9][0-9]-conquest|h76-yield|h77-chords) continue ;;
    esac
    case "$name" in
        c[0-9]*-conquest) ;;
        *) continue ;;
    esac
    size=$(stat -c %s "$archive")
    sha=$(sha256sum "$archive" | cut -d' ' -f1)
    printf '{"file":"archive-live.json","sha256":"%s","bytes":%s,"compressed":"archive-live.json.zst"}\n' \
        "$sha" "$size" > "$dir/archive-manifest.json"
    if nice -n 15 zstd -T$threads -q --rm "$archive"; then
        after=$(stat -c %s "$archive.zst")
        compressed=$((compressed+1))
        freed=$((freed+size-after))
        echo "OK $name $((size/1000000))MB -> $((after/1000000))MB"
    else
        failed=$((failed+1))
        rm -f "$dir/archive-manifest.json"
        echo "FAILED $name"
    fi
done
echo "COMPRESS_FILES=$compressed"
echo "COMPRESS_FAILED=$failed"
echo "COMPRESS_FREED_GB=$((freed/1000000000))"
df -h /root | tail -1
echo "COMPRESS_SENTINEL: DONE"
