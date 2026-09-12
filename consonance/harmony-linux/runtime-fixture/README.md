# Runtime fixture

This tiny OCI application exercises the production guest runtime rather than
application-specific behavior. It checks argv, environment, working directory,
external input, nonroot credentials, SDK transport, system entropy, virtual time,
and a kernel-owned observation mapping. Setup and two execution boundaries let
the host capture state and compare a restored continuation. The host oracle must
also check that execution reaches the final completion event.

Build the static binary on the target Linux architecture and package it using
`package.py`. The fixture has no registry dependency. Its image uses uid/gid 1000,
while the platform supervisor remains responsible for lifecycle and management.
The external file `/input/data` must contain `platform-input` followed by newline.

The fixture requires a real platform guest. Building it or checking archive
contents does not constitute SDK, time, or snapshot qualification.
