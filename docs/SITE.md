# Documentation site

The public site serves people preparing applications, finding bugs, and
investigating saved findings. Navigation follows that journey. Tutorials use a
real application property; reference describes exact interfaces; component
READMEs remain the authority for implementation details.

## One example, two consumers

`docs/examples/*.sh` contains named example regions. Pages include a region with
`{{ example "name" }}`. The MkDocs hook renders the exact bytes that
`scripts/docs_examples.py` executes. Shell input is also a named region and is
fed to the documented shell command. The counter recipe and investigation
script are included directly from files copied unchanged into that execution.
No copied code fences are allowed in page sources. The strict build rejects
unknown regions, untested examples, unpublished regions, or source files without
an execution owner. `docs/examples/catalog.json` registers every step, expected
exit status, optional input, time bound, and result predicate.

The command reference comes from the built executable's help, including its
public subcommands. It has no hand-maintained option table. Inline command names
and option names in prose are also checked against this help. Prose still needs
review when meaning changes; the checks protect executable contracts rather
than claiming to prove every sentence.

## Checks

- The documentation workflow runs on every PR and main push, including code-only
  changes. It executes the source-build example, tests the example harness, and
  builds MkDocs strictly with help from that executable.
- The Documentation Examples job in the language workflow executes the full
  application walkthrough against matching runtime artifacts and the standard
  C image preparation. Relevant runtime, searcher, workload, CLI, or example
  changes select it. Failure to prepare or find the expected confirmed assertion
  is a failure, not a skip. No fixed seed is required to reach the finding.
- The walkthrough checks actual finding evidence, restoration of guest files
  from command and interactive branches, a search from the shell-modified state,
  and additional execution work on resume. It retains per-step logs and hashes
  of the exact displayed commands, plus result manifests and reports. CI uploads
  those compact records; it does not duplicate guest images and memory snapshots
  in the documentation artifact. It never substitutes a mock for a guest run.
- Unit tests plant untested snippets, copied fences, false findings, and a resume
  that does no work. These must be rejected.

Run the fast checks with `python3 scripts/docs_examples.py lint` and
`python3 -m unittest discover -s scripts -p test_docs_examples.py`.
Run the integration with `python3 scripts/docs_examples.py application --evidence
/tmp/harmony-docs-evidence`. Supply a built CLI through `HARMONY_BINARY` and the
matching guest directory through `HARMONY_DOCS_GUEST_DIR` when not using the
checkout's default locations. Evidence directories must be fresh.

The integration supplies source-build outputs in an isolated checkout and runs
the published installation example to copy the executable and matching guest
artifacts into `.harmony-install`. It does not change the tutorial
recipe or hide runtime overrides in the commands. Docker or Podman and KVM are
required for the CI lane; other platform support is not inferred from this lane.

## Preview

Install `docs/requirements.txt` in a Python virtual environment. Build the CLI,
set `HARMONY_BINARY` if needed, and run `python -m mkdocs serve`. A strict site
build is `python -m mkdocs build --strict`. Inspect desktop and mobile navigation,
search, code copying, and both color schemes after layout changes.

`mkdocs.yml` owns navigation and the project-site URL. The hook is `docs/hooks.py`.
Only `docs/user/` is published; site-maintenance and contributor documentation
stay outside it. Search is local and the site loads no external fonts or analytics.

## Publishing

Pull requests upload reviewable HTML without publishing it. Only main-branch
pushes and manual runs on main can deploy through the `github-pages` environment.
Publishing rebuilds current main inside one shared concurrency slot, preventing
an older workflow from publishing stale content. The workflow and its permission
boundary are registered in `scripts/ci_contract.py`.

Pages must use the GitHub Actions source, and the Pages environment must allow
main. The public site is https://ph14.github.io/harmony/. Merge only after the
Documentation and Documentation Examples checks pass; publication itself is not
a substitute for the guest integration check.
