# Documentation site

The public site is built only from `docs/user/`. Its audience is people installing
Harmony and testing their own applications. Architecture, contributor workflows,
CI details, design notes, and site maintenance stay outside that directory.
The four navigation sections follow [Diátaxis](https://diataxis.fr/): tutorials
teach through a bounded exercise, how-tos solve a task, reference defines an
interface, and explanation develops understanding. Do not mix those purposes
into a single catch-all guide.

## Preview and verification

```sh
python3 -m venv /tmp/harmony-docs
/tmp/harmony-docs/bin/pip install -r docs/requirements.txt
/tmp/harmony-docs/bin/mkdocs build --strict
/tmp/harmony-docs/bin/mkdocs serve
```

Open the local URL printed by MkDocs. The strict build rejects missing pages,
links, and anchors. Check desktop/mobile layout, navigation, search, code-copy
controls, and both color schemes when changing the template. The site has no
analytics or externally hosted fonts; search runs in the browser.

`mkdocs.yml` owns the public navigation and project-site base URL. Custom styles
and the SVG mark are in `docs/user/assets/`. Generated HTML goes to ignored
`site/`. `docs/requirements.txt` pins the renderer and theme; keep MkDocs on the
validated 1.x version when updating dependencies.

## Publishing

`Release / Harmony / Documentation` builds the strict site on pull requests and
pushes to main. Pull requests upload a downloadable HTML artifact without
publishing it. Only a main-branch push or manual dispatch on main can upload a
Pages artifact and deploy, through the `github-pages` environment. Deployment
permissions are scoped to that job. The workflow is registered in
`scripts/ci_contract.py`.

The repository's Settings → Pages source must be **GitHub Actions** (`build_type:
workflow`). The Pages environment must permit deployments from main. This
one-time repository setting is separate from the workflow; `configure-pages`
fails visibly if the repository is not configured. The public URL is
<https://ph14.github.io/harmony/>. Merging a PR that changes site inputs builds
and deploys the site; no generated-content branch or custom domain is needed.

## Content verification

Check CLI options against `cli/src`, image semantics against `consonance/oci`,
bundle/checker behavior against the supervisor, and SDK examples against the
guest SDK and Linux device transport. Use executable examples where practical.
Distinguish a source/compile check from a real guest run. Do not invent release
assets, package-manager distribution, or platform qualification from build-only
evidence. Keep current limitations explicit without importing implementation
history into the end-user navigation.
