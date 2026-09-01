# PROTOCOL — the wire-protocol authority doc

This document is the authority for the control protocol: what its verbs are, which plane each
belongs to, and what obligation that plane carries. It records a ruled *direction* for the wire
as well — but **this document changes no bytes**. Executing the collapse described under "Ruled
direction" is a separate PR, gated on the protocol tests that landed alongside this doc.
Companion docs: `docs/TESTING.md` (rung 4 is where these obligations become tests),
`docs/GLOSSARY.md` (the naming authority).

## What the control protocol is

harmony's engine can be driven in-process, but the interesting clients are remote: a campaign
runner, an interactive resolution session, a recording tool. They speak a small binary protocol
over a byte stream — `dissonance/control-proto` defines the frames and the value types,
`vmm_core::control::ControlServer` serves them against a live VM.

A **session** is one transport lifetime (server ↔ client). It is orthogonal to a campaign and is
never a synonym for one.

The protocol has **14 peer verbs** today. Listing them is not a design; classifying them is.
Grouping the verbs into planes, and attaching **one obligation per plane**, is what makes the wire
reviewable: a new verb inherits its plane's obligation, and a verb that cannot honor the
obligation of any plane is a design error rather than an implementation detail.

---

## The five planes

### 1. Session — `Hello`

**Verb.** `Hello(Caps)` → `Reply::Hello(Caps)`.

**What it is.** The capability and version handshake. It must be the first verb of a session:
before it, nothing has been negotiated, so everything else answers `Unsupported`. Both sides
exchange a `Caps` — protocol version, the range of reproducer-blob versions understood, the
coverage-map geometry, and the capability flags — and each compares for itself. Version
incompatibility is therefore *detectable from `Caps` alone*; the server does not need an error
reply to express it.

**Obligation: the handshake is first, and mismatch is visible without a failed operation.** A
client must never discover an incompatibility by having a later verb misbehave.

### 2. State algebra — `Snapshot`, `Branch`, `Run`, `Replay`, `Drop`

**Verbs.**

| Verb | Meaning |
|---|---|
| `Snapshot` | capture state at a quiescent point → the seal-bound reply: the pool-wide handle, the synchronized seal `Moment`, the timeline taint bit, and the seal's evidence cut |
| `Branch { snap, env }` | restore `snap` into a fresh VM and reseed from `env` — the explore path |
| `Replay(snap)` | restore `snap` verbatim — the reproduce path |
| `Run { until, resolve }` | advance the VM to a stop condition → a `StopReason` |
| `Drop(snap)` | release a snapshot handle (pool GC) |

**What it is.** The deterministic core. These five verbs are the algebra of states and
transitions: a snapshot is a value, branch and replay are the two ways to get back to one, run is
the transition, drop is the deallocation. Everything else in the protocol is either a question
about the machine or a way to change what the machine is running.

**Obligation: replay identity.** Restoring a snapshot and re-running the same inputs must
reproduce the same state, bit for bit. This is not one obligation among several — it is the
property the entire project exists to provide, expressed on the wire. Every other plane's
obligation exists to keep *this* one true.

Consequences that follow from it, and are therefore not separate rules: a `Drop`ped snapshot is no
longer branchable (a handle that outlives its state could reproduce nothing); a double-drop and a
dangling handle are errors rather than silent successes (a client that believes it holds state it
does not hold will mint reproducers that do not reproduce); handles are never reused (a later
snapshot must not inherit an earlier one's identity, or its taint).

### 3. Observation — `Hash`, `Read`, `Regs`, `Console`, `SdkEvents`

**Verbs.**

| Verb | Meaning |
|---|---|
| `Hash { scope }` | a canonical 32-byte digest of the machine, at the requested scope |
| `Read { gpa, len }` | `len` bytes of guest-physical memory |
| `Regs` | a versioned register *view* (not the save/restore format — additive evolution, no round-trip obligation) |
| `Console { offset }` | a page of the guest serial capture |
| `SdkEvents { offset }` | a page of the `Moment`-stamped SDK event capture |

**What it is.** Questions. None of them is a move: an observation reads the machine and is never
recorded into a reproducer, because replaying a question would change nothing.

**Obligation: hash neutrality.** *Observing must not change the machine.* A run with observations
interleaved into it must end at exactly the same `state_hash` as the same run with no observations
at all. This is the obligation that makes an interactive session safe: a human poking at a
timeline must not be able to invalidate it, and a debugging session must not silently become
un-reproducible.

Hash neutrality also bounds what may join this plane. A verb that must mutate to answer does not
belong here, however read-like it looks.

Two structural notes that fall out of the plane, and that the tests pin:

- **Paging is a property of the answer, not of the machine.** `Console` and `SdkEvents` are paged
  because a capture can exceed the frame limit; the cursor lives in the client's request, never in
  server state, so paging cannot perturb anything.
- **An over-range read is an error, never a truncated success.** A short answer that looks like a
  successful answer is how a client silently builds a wrong model of the guest.

### 4. Intervention — `Perturb`, `Exec`

**Verbs.**

| Verb | Meaning |
|---|---|
| `Perturb { fault, at }` | stage a host-plane fault to be applied at `Moment` `at` |
| `Exec { cmd, deadline }` | inject a command on the guest's serial input and capture the output |

**What it is.** The two ways a client changes what the machine is running rather than asking about
it. They look similar and their obligations are **opposite**, which is precisely why they share a
plane: putting them side by side is what stops the difference from being forgotten.

**Obligation, `Perturb`: it is recorded.** A fault is an **input**. Staging one is therefore
*environment amendment* — the fault is stamped into the active recorded reproducer at its
`Moment`, and replaying that reproducer re-applies the identical schedule at the identical counts.
A host fault that were not recorded would be a deterministic engine producing an unreproducible
run, which is a contradiction, not a limitation.

**Obligation, `Exec`: it is explicitly off the record, and says so structurally.** The serial byte
channel is deliberately crude; an improvisation carries no determinism guarantee and is never
recorded into any reproducer. What *is* airtight is the taint guard around it: the first `Exec`
against a timeline sets that timeline's taint bit, every snapshot taken from it reports itself
tainted, and minting a reproducer from a tainted timeline is a loud error rather than a handle
that does not reproduce. The server refuses nothing — a caller may deliberately sacrifice a
timeline — but the consequence is structural, not conventional.

**`Exec` belongs behind a debug capability.** It is an interactive-debugging affordance, not part
of the production drive path, and a client that never intends to improvise should not be able to
taint a timeline by accident. See the ruled direction below.

### 5. Provenance — `RecordedEnv`

**Verb.** `RecordedEnv` → `Reply::Recorded(Reproducer)`: the genesis-complete reproducer that
replays the current point, or a loud `Tainted` error if the timeline has been improvised on.

**What it is.** The mint. It converts the *live* currency (a state, expensive and transient) into
the *portable* one (a reproducer, cheap and durable) — the identity `state = replay(reproducer)`
made into a wire verb.

**Obligation: golden-stable encoding.** Reproducers are **persisted evidence**. A reproducer
recorded today is expected to replay months from now, from an archive, on a different machine, to
justify a bug report. Codec drift therefore does not merely break a client — it silently
invalidates the archive, and it does so without any test failing unless the *bytes themselves* are
pinned. That is why the provenance obligation is stated as an encoding property rather than a
round-trip property: a codec that round-trips its own drift is exactly the failure mode.

---

## The three-hash pin

Three names, three roles, near-identical vocabulary. **Never unify them.**

| Name | What it covers | Where it lives |
|---|---|---|
| `state_hash` | **all** architectural state, latent state included — registers, RAM, device models, the seed-derived entropy stream | `Subject::state_hash`, `Vmm::state_hash` |
| `observable_digest` | **only** guest-emitted output — the bytes the guest deliberately emits (report stream, serial), carrying no latent device or PRNG state | `Subject::observable_digest` |
| `Hash { scope }` | the **wire verb** that asks for a digest, scoped by `HashScope` | `control-proto` |

The distinction is load-bearing for the **O3 seed-sensitivity** oracle, and only for a reason that
is easy to get backwards: two runs at different seeds will diverge `state_hash` *whatever the
payload does*, because the seeded entropy stream is part of the state. So `state_hash` cannot
distinguish "this payload's behavior depends on the seed" from "this payload was handed a
different seed" — it answers "diverged" for both. **`observable_digest` is the only sound basis
for O3.** A payload that consumes randomness without branching on it must keep an identical work
count across seeds while its *observable output* diverges; that statement is only checkable
against the observable digest.

`Hash { scope }` is neither of the other two: it is the request. Its scope selects what the server
digests. Collapsing it into either digest's name would make it impossible to say "ask for the
observable digest over the wire" without ambiguity.

---

## Ruled direction — recorded, not executed here

The following changes to the wire are **ruled**, and are **not made in this PR**. Executing them
is a separate PR, **gated on the protocol tests landed alongside this document** — the tests are
what make the collapse safe to perform, so they come first.

1. **Collapse the five observation verbs into one `Observe { scope }`.** `Hash`, `Read`, `Regs`,
   `Console`, and `SdkEvents` are five spellings of one operation with five scopes. One verb with
   a scope makes the plane's obligation checkable in one place — a new observation kind becomes a
   new scope variant that inherits hash neutrality by construction, instead of a new verb whose
   author must remember the rule.
2. **Fold `Perturb` into environment amendment.** `Perturb` already *is* environment amendment
   (that is its obligation); it should be spelled as such, so the wire says what it does rather
   than naming the effect on the guest.
3. **Gate `Exec` behind a debug capability.** Negotiated at `Hello`, off by default. A client that
   does not ask for improvisation cannot taint a timeline.
4. **Rename `RecordedEnv` → `Reproduce`.** The reply already carries a `Reproducer`; the request
   should use the ruled word (`docs/GLOSSARY.md`) rather than the retired one. The verb answers
   "give me the thing that reproduces this point."

None of the four is a behavior change. All four are places where the current wire spells a ruled
concept with a pre-ruling word, or spends five verbs where the obligation is one.
