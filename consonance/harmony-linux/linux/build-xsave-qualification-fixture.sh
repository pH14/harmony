#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
if [ "$#" -ne 2 ]; then
    echo "usage: $0 VERIFIED_GUEST_OUTPUT EMPTY_OUTPUT" >&2
    exit 2
fi
here=$(cd "$(dirname "$0")" && pwd)
source_dir=$(cd "$1" && pwd)
mkdir -p "$2"
output=$(cd "$2" && pwd)
[ -z "$(find "$output" -mindepth 1 -maxdepth 1 -print -quit)" ] || {
    echo "FAIL: qualification output must be empty" >&2
    exit 1
}
(cd "$source_dir" && sha256sum -c MANIFEST.sha256)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/tree/bin" "$work/tree/proc" "$work/tree/dev"
gzip -dc "$source_dir/x86_64/initramfs.cpio.gz" | \
    cpio --quiet -i --to-stdout bin/busybox >"$work/tree/bin/busybox"
test -s "$work/tree/bin/busybox"
cc -static -O2 -Wall -Wextra -Werror "$here/xsave-guest-check.c" -o "$work/tree/check"
for executable in "$work/tree/bin/busybox" "$work/tree/check"; do
    readelf -h "$executable" | grep -q 'Advanced Micro Devices X86-64'
    if readelf -l "$executable" | grep -q INTERP || readelf -d "$executable" | grep -q NEEDED; then
        echo "FAIL: dynamic qualification executable: $executable" >&2
        exit 1
    fi
done
ln -s busybox "$work/tree/bin/sh"
cat >"$work/tree/init" <<'INIT'
#!/bin/sh
set -e
export LD_BIND_NOW=1
/bin/busybox mount -t proc proc /proc
/check
/bin/busybox poweroff -f
INIT
chmod 0755 "$work/tree/init" "$work/tree/check" "$work/tree/bin/busybox"
find "$work/tree" -exec touch -h -d @0 {} +
(cd "$work/tree" && find . -print0 | LC_ALL=C sort -z | \
    cpio --null -o --format=newc --owner=0:0 --reproducible --quiet) | gzip -n -9 >"$output/initramfs.cpio.gz"
cp "$source_dir/x86_64/bzImage" "$output/bzImage"
cp "$source_dir/x86_64/bzImage.vmlinux" "$output/vmlinux"
cp "$source_dir/MANIFEST.sha256" "$output/source-guest-manifest.sha256"
cp "$work/tree/check" "$output/xsave-guest-check"
{
    printf 'source_commit=%s\n' "$(git -C "$here" rev-parse HEAD)"
    printf 'check_source_sha256=%s\n' "$(sha256sum "$here/xsave-guest-check.c" | cut -d' ' -f1)"
    printf 'compiler=%s\n' "$(cc --version | head -1)"
    printf 'busybox_sha256=%s\n' "$(sha256sum "$work/tree/bin/busybox" | cut -d' ' -f1)"
    printf 'fixture_scope=G1-functional-and-whole-endpoint-qualification\n'
} >"$output/provenance.txt"
python3 - "$output" <<'PY_SITES'
import json
from pathlib import Path
import re
import subprocess
import sys
out = Path(sys.argv[1])
sites = []
for function in ("save_fpregs_to_fpstate", "copy_fpstate_to_sigframe"):
    dump = subprocess.check_output(["objdump", "-dw", "--disassemble=" + function, str(out / "vmlinux")], text=True)
    (out / (function + ".asm")).write_text(dump)
    lines = []
    for line in dump.splitlines():
        match = re.match(r"^\s*([0-9a-f]+):\s+(?:[0-9a-f]{2}\s+)+\s*([a-z][a-z0-9]*)\s*(.*)$", line)
        if match:
            lines.append((int(match[1], 16), match[2], line.strip()))
    saves = [i for i, item in enumerate(lines) if item[1] in ("xsave", "xsave64")]
    if not saves:
        raise SystemExit("FAIL: no plain XSAVE found in " + function)
    # The dedicated Harmony branch follows the generic signal branch in this
    # pinned source. A missing runtime hit fails qualification, not skips it.
    for index in saves[-1:]:
        masked = next((i for i in range(index + 1, min(index + 80, len(lines)))
                       if re.search(r"\band\s+\$0x7,", lines[i][2])), None)
        stored = next((i for i in range(masked + 1, min(masked + 8, len(lines)))
                       if re.search(r"\bmov\s+%[a-z]+,-0x[0-9a-f]+\(%rbp\)", lines[i][2])), None) if masked is not None else None
        if stored is None:
            raise SystemExit("FAIL: bitmap stack store shape requires review: " + function)
        for position, label in ((index, "before-save"), (index + 1, "after-save"), (stored + 1, "after-bitmap-spill")):
            if position >= len(lines):
                raise SystemExit("FAIL: missing successor instruction")
            address, _, instruction = lines[position]
            sites.append({"function": function, "phase": label, "rip": f"0x{address:x}", "instruction": instruction})
(out / "debug-sites.json").write_text(json.dumps(sites, indent=2) + "\n")
(out / "debug-rips.txt").write_text(",".join(dict.fromkeys(s["rip"] for s in sites)) + "\n")
PY_SITES
(cd "$output" && sha256sum bzImage initramfs.cpio.gz vmlinux xsave-guest-check source-guest-manifest.sha256 provenance.txt debug-sites.json debug-rips.txt ./*.asm >MANIFEST.sha256)
echo "ok: $output"
