# SPDX-License-Identifier: AGPL-3.0-or-later
"""Produce a source-only upstream patch from explicitly reviewed tile edits."""

import argparse
import difflib
import hashlib
import json
import subprocess
import xml.etree.ElementTree as ET
from pathlib import Path


def edit_tmx(text, edits):
    root = ET.fromstring(text)
    layer = min((item for item in root.findall("layer") if item.get("name", "") in ("", "BG")),
                key=lambda item: int(item.get("id")))
    data = layer.find("data")
    assert data.get("encoding") == "csv"
    width, height = int(layer.get("width")), int(layer.get("height"))
    old = data.text
    lines = old.splitlines(keepends=True)
    rows = [index for index, line in enumerate(lines) if line.strip()]
    assert len(rows) == height
    seen = set()
    for edit in edits:
        x, y = edit["column"], edit["row"]
        assert isinstance(x, int) and isinstance(y, int) and 0 <= x < width and 0 <= y < height
        assert (x, y) not in seen, "duplicate tile edit"
        seen.add((x, y))
        index = rows[y]
        line = lines[index]
        values = line.strip().rstrip(",").split(",")
        assert len(values) == width
        assert int(values[x]) == edit["from_gid"], f"unexpected tile at ({x}, {y})"
        assert 0 < edit["to_gid"] < 2**29, "flipped/rotated tile GIDs are unsupported"
        values[x] = str(edit["to_gid"])
        lines[index] = ",".join(values) + ("," if line.rstrip().endswith(",") else "") + ("\n" if line.endswith("\n") else "")
    return text.replace(old, "".join(lines), 1)


def prepare(source, spec):
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=source, text=True).strip()
    assert revision == spec["target_revision"], "upstream revision changed: review the edits again"
    patch, manifest = [], []
    for entry in spec["levels"]:
        path = Path(entry["path"])
        assert not path.is_absolute() and ".." not in path.parts and path.suffix == ".tmx"
        assert path.parts[:2] == ("LEVELS", "LEVEL DATA")
        subprocess.run(["git", "ls-files", "--error-unmatch", "--", path.as_posix()], cwd=source,
                       stdout=subprocess.DEVNULL, check=True)
        assert subprocess.run(["git", "diff", "--quiet", "HEAD", "--", path.as_posix()], cwd=source).returncode == 0
        old = (source / path).read_text()
        assert hashlib.sha256((source / path).read_bytes()).hexdigest() == entry["source_sha256"]
        new = edit_tmx(old, entry["edits"])
        assert new != old, "patch has no changes"
        a, b = json.dumps("a/" + path.as_posix()), json.dumps("b/" + path.as_posix())
        patch.append(f"diff --git {a} {b}\n")
        patch.extend(difflib.unified_diff(old.splitlines(True), new.splitlines(True), fromfile=a, tofile=b))
        manifest.append({"path": path.as_posix(), "before_sha256": entry["source_sha256"],
                         "after_sha256": hashlib.sha256(new.encode()).hexdigest(), "edits": entry["edits"]})
    return "".join(patch), {"schema": 1, "target_revision": revision, "changes": manifest,
                             "status": "source-proposal-needs-export-build-and-replay",
                             "evidence": spec.get("evidence", [])}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("edits", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    patch, manifest = prepare(args.source, json.loads(args.edits.read_text()))
    args.output.write_text(patch)
    args.output.with_suffix(".manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    subprocess.run(["git", "apply", "--check", str(args.output.resolve())], cwd=args.source, check=True)
    print(f"Prepared {len(manifest['changes'])} reviewed source edits; exporter/build/replay remain required")
