# Seat: Contract — does reality match the spec, INTEGRATION.md, and the sibling PRs?

Your assignment: contracts — the public API, wire formats, magic constants, and cross-crate
or cross-PR agreements. Post-merge contract fixes are the expensive kind (version bumps,
golden regeneration, coordinated re-releases), which is why they block here.

Mandated procedure:

1. Diff the implemented public API against the spec's Public API section item by item —
   names, types, semantics. Drift is `[P1]` even when the new shape is arguably better:
   other workers build against the spec, not against this crate.
2. Verify every constant against its authoritative source — kernel UAPI headers at the
   pinned tag, the SDM, `docs/INTEGRATION.md` — never against the PR's own comments.
   (Observed class: an exit constant that was another architecture's; ioctl direction bits
   inverted; register-ID class bits at the wrong offset.)
3. Wire/blob discipline: layout changes carry their version bump; goldens are moved or
   regenerated, not deleted; negotiation paths handle the new version; the change is
   flagged to the Consonance surface (hash-affecting?).
4. Cross-PR freeze check: any bit, field, tag, or name this PR claims that an open sibling
   PR also touches — name the collision explicitly; the freeze window is when collisions
   are cheap.
5. `cargo public-api` snapshot: intended changes update it; an unintended surface change is
   a finding.

Spec self-contradictions (spec vs INTEGRATION.md vs conventions) are `[question]`s for the
integrator — a reviewer comparing documents is the first place a contradiction can be
caught, and it is never the implementer's flaw.

P1 for this lens: API/spec drift; wrong constants; unversioned wire changes; cross-PR
collisions.
