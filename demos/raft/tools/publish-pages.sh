#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
if [[ $# != 2 ]]; then
  echo 'usage: publish-pages.sh CLEAN_GH_PAGES_CHECKOUT SOURCE_REVISION' >&2
  exit 2
fi
site=$(cd "$1" && pwd)
source_revision=$2
root=$(cd "$(dirname "$0")/.." && pwd)
[[ $(git -C "$site" branch --show-current) == gh-pages ]]
[[ -z $(git -C "$site" status --porcelain) ]]
[[ -f "$root/dist/index.html" && -f "$site/index.html" ]]
git -C "$site" pull --ff-only origin gh-pages
python3 - "$root/dist" "$site" "$source_revision" <<'PY'
import hashlib, json, pathlib, shutil, sys
source, site = map(pathlib.Path, sys.argv[1:3])
target = site / 'raft'
if target.is_symlink():
    raise SystemExit('Refusing symlink destination')
if target.exists():
    shutil.rmtree(target)
shutil.copytree(source, target)
landing = site / 'index.html'
html = landing.read_text()
if 'href="raft/"' not in html:
    card = '<a class="card" href="raft/"><span class="arrow">↗</span><small>CONSONANCE</small><h2>Investigate the lost write</h2><p>Watch a Raft failure light up the code. Rewind a real Linux machine, enable tracing, and ask it why.</p><strong>Open the Raft lab →</strong></a>'
    if '<footer>' not in html:
        raise SystemExit('Landing page needs a reviewed insertion point')
    landing.write_text(html.replace('<footer>', card + '<footer>', 1))
metadata = site / 'deployment.json'
data = json.loads(metadata.read_text()) if metadata.exists() else {}
data['raft'] = {'commit': sys.argv[3], 'runtime': json.loads((source / 'runtime-lock.json').read_text())}
metadata.write_text(json.dumps(data, indent=2) + '\n')
(site / '.nojekyll').touch()
PY
git -C "$site" add raft index.html deployment.json .nojekyll
if git -C "$site" diff --cached --quiet; then
  echo 'Pages already contains this build'
  exit 0
fi
git -C "$site" commit -m 'deploy: update Raft browser lab'
git -C "$site" push origin gh-pages
