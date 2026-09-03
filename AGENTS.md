# Working in Harmony

Harmony builds tools for exploring and reproducing software failures through
deterministic execution.

## Working approach

- Make the smallest coherent change that fully addresses the request.
- Use the current code, executable checks, and nearby README as the source of
  truth for each component.
- Update the nearby README when a change affects a component's purpose,
  architecture, boundaries, or usage.
- Preserve unrelated work and surface meaningful scope expansions before
  undertaking them.
- Use repository scripts and CI configuration to determine the checks relevant
  to changed code.
- Document the safety invariant beside every `unsafe` block and exercise unsafe
  logic under Miri.
- Record follow-up work in GitHub issues and preserve implementation history in
  commits and pull requests.
- For code-review work, use the applicable lenses in `REVIEWING.md`.

## Maintaining these instructions

Keep this file short and stable. Place component knowledge in component
READMEs, enforceable rules in tooling, review criteria in `REVIEWING.md`, and
historical context in Git. Consolidate existing guidance when adding something
new.
