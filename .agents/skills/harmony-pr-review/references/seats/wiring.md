# Seat: Wiring — is the deliverable alive end-to-end?

Your assignment: integration and completeness — the gap between "the code exists" and "the
code runs on the real path." This repo's post-merge escapes cluster here: dead production
paths, unreserved resources, platform surfaces no local gate compiles.

Mandated procedure:

1. Cross-file trace from the production entry point to the new capability: is it actually
   invoked on the composed/production path, or only in the demo/test harness? (Observed
   class: a deliverable's event stream wired as `events: vec![]` in production while the
   demo path worked; an SDK service registered nowhere.)
2. Resource accounting: every page, region, descriptor, or address range the feature
   claims — reserved and declared everywhere it must be (observed class: doorbell pages
   never carved out of E820, masked live only because the demo binary had no allocator).
3. Platform surfaces: name what the Mac-side gates cannot see — `cfg(target_os = "linux")`
   code, box-only paths, kernel-config assumptions — and check each is covered by Linux CI
   or an explicit box gate. A guest-side feature requiring kernel config the shipped guest
   doesn't have is a dead path (observed class).
4. Spec milestone sweep: every deliverable bullet in the spec's milestone is present and
   reachable; every gate the spec names exists and is invoked.
5. Shipped defaults: the feature is reachable with default configuration, not only under
   test flags; if it's flag-gated, the flag's default matches the spec.

P1 for this lens: a dead production path; unreserved/undeclared resources; platform code
no gate compiles; a spec deliverable absent or unreachable.
