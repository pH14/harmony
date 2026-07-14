# Task 106 — harmony-cloud-vendor-cli: budget-gated leases on cloud & bare-metal machines

**Paul's rulings (2026-07-13):** new standalone repo named `harmony-cloud-vendor-cli`;
**no Terraform** — provider access is shelling out to official CLIs (`gcloud`, `aws`,
`hcloud`) plus plain REST where no CLI is ubiquitous (phoenixNAP, Hetzner Robot); plain
descriptive naming throughout (the GLOSSARY harmony-theory register applies to product
concepts, NOT to infra tooling — do not propose whimsical names).

**What this is.** The multiarch + nested-virt programs (docs/ARM-PORT.md, tasks/100,
tasks/102-amd-vendor-spike-doc.md, docs/NESTED-X86.md) need regular, short-lived access
to bare-metal Intel/AMD/ARM machines and nested-virt-capable VMs. This is a personal
project with a hard spend ceiling (~$10–15/day). The tool makes every machine a **lease**:
born with an expiry tag, killed by an authoritative sweeper, refused at creation if the
daily budget would be exceeded, and all of it killable from one scram button. The foreman
gets verbs, not provider consoles; the budget gate is enforced by the tool, not by
foreman judgment.

**Where work happens:** the new repo (Paul creates it; see Prerequisites). This spec
lives in harmony for the record. The tool has zero dependencies on the harmony workspace
in either direction. Normal PR review discipline applies in the new repo.

## Ruled decisions (binding — do not relitigate in implementation)

1. **Repo:** `harmony-cloud-vendor-cli`, standalone. Rust crate, single binary. Binary
   name default `hcv` (Paul may override at review; make it a one-line change).
2. **No Terraform, no state files.** Leases live in provider tags/labels. `ls`, `sweep`,
   and `budget` are computed from live provider inventory + the ledger (below). Any
   machine, runner, or laptop gets the same answer.
3. **Drivers v1:** `gcp` (gcloud), `aws` (aws CLI), `pnap` (phoenixNAP REST,
   OAuth client-credentials), `hetzner` (hcloud CLI). **Oracle Cloud is not used — no
   OCI driver, ever** (Paul 2026-07-13). Design the driver trait so a fifth driver is
   cheap to add: the AMD-provider selection (bead hm-5wq) will bring one
   (Vultr/Scaleway/Latitude.sh).
4. **Budget:** hard daily cap, default **$15** (config). Charges are **pessimistic**:
   the full lease duration at the profile's **on-demand** rate is charged at `lease`
   time, even for `--spot`. `lease`/`extend` fail closed when the projected day total
   exceeds the cap.
5. **Destructive verbs (`release`, `sweep`, `scram`) act ONLY on resources carrying the
   managed tag.** Never account-wide. There is NO Hetzner Robot integration at all —
   the harmony determinism box is a Robot dedicated server, and the tool only ever holds
   an hcloud token scoped to a dedicated empty Hetzner Cloud project, which cannot see
   Robot servers by construction. The box is invisible to this tool, not merely
   protected from it.
6. **Two payloads:** `--runner` (ephemeral GitHub self-hosted runner registered to a
   target repo, labels from the profile) and `--dev` (SSH key injected, endpoint
   printed). Same lease/TTL/sweeper semantics for both.
7. **Secrets and control-plane workflows live in the tool repo, never in harmony.**
   Harmony's workflows only ever see the runner side.

## CLI surface

```
hcv bootstrap <provider> [--check]   # idempotent one-time setup: scoped IAM/SG/ssh-key/API-enable
hcv profiles                         # list profiles from profiles.toml
hcv lease <profile> --hours N --task <bead-id> (--runner <owner/repo> | --dev) [--spot]
hcv ls                               # live leases, ALL providers: id, profile, task, lease left, accrued $
hcv ssh <lease-id>                   # exec ssh into a --dev lease
hcv extend <lease-id> --hours N      # budget-checked
hcv release <lease-id>               # terminate now, post ledger event
hcv sweep                            # kill past-lease, report orphans; what the cron calls
hcv budget [--json]                  # today's spend: ledger events + live pessimistic charges
hcv scram                            # kill ALL managed resources on ALL providers; set HCV_ENABLED=false
```

Every verb: `--json` output alongside the human table; idempotent and safe to re-run
(agents retry). Exit codes are part of the contract — foreman scripts branch on them:
`0` ok · `2` budget-refused · `3` kill-switch disabled (HCV_ENABLED=false) ·
`4` provider capacity/quota failure · `5` spot interruption detected ·
`6` ledger unreachable (fail-closed) · `1` everything else. A spot interruption is a
distinct outcome, never reported as a gate/workload failure.

`--dry-run` on every mutating verb prints the exact provider commands/requests without
executing. This is the primary review surface — golden-test it.

## Tag schema

Canonical keys (values UTC): `hcv-managed=true`, `hcv-lease-until=<epoch-seconds>`,
`hcv-task=<bead-id>`, `hcv-profile=<name>`, `hcv-created-by=<user|workflow-run-id>`.

Per-provider mapping — encode exactly, these are mid-implementation traps:
- **AWS:** instance tags, applied atomically via `TagSpecifications` on `run-instances`.
- **GCP:** labels at insert time. GCP label values allow only `[a-z0-9_-]`, ≤63 chars —
  hence epoch seconds, never ISO-8601, and keys use `hcv_lease_until` style underscores.
- **pnap:** the `tags` field in the provision request (create tags in the pnap Tag
  Manager during `bootstrap`).
- **hcloud:** labels at create.

**Invariant: tags are applied in the same API call that creates the resource.** A
create-then-tag sequence that crashes in between leaks an untagged box the sweeper
cannot see. There must be no code path that creates untagged.

## profiles.toml

Checked into the tool repo. Schema per entry: `provider`, `shape`, `region`, `image`,
`on_demand_rate_usd` (+ `rate_source_url`, `rate_checked` date), `spot_allowed`,
`nested_virt` (provider-specific enable), `min_cpu_platform` (GCP), `runner_labels`,
`notes`. Cap math always uses `on_demand_rate_usd`.

Seed entries (rates are planning numbers from 2026-07-13 research — re-verify each
against the provider pricing page at implementation time and fill `rate_checked`):

| profile | provider | shape | ~$/hr od | notes |
|---|---|---|---|---|
| `nested-gcp-intel` | gcp | n2-standard-8, spot, `--enable-nested-virtualization`, min platform Ice Lake | 0.39 | everyday nested-x86 surface; L1 must be Linux KVM (GCP restriction) |
| `nested-gcp-intel-smoke` | gcp | n2-standard-4, spot, nested | 0.20 | cheap smoke shape |
| `nested-gcp-intel-old` | gcp | n1-standard-8, nested | 0.38 | older-VMX data point |
| `nested-gcp-intel-new` | gcp | c3-standard-8, nested | 0.45 | Sapphire Rapids canary |
| `nested-aws-intel` | aws | c7i.2xlarge, `--cpu-options NestedVirtualization=enabled` | 0.36 | second L0 (Nitro). Supported families: C7i/M7i/R7i(+flex), C8i/M8i/R8i(+id/flex), X8i, I7i — Intel only |
| `arm-altra-metal` | pnap | a1.c5.large (Altra 80c, HPE RL300) | **TBD — confirm in BMC portal (M3)** | the ARM spike target machine; flag to Paul if > $2/hr |
| `arm-graviton2-metal` | aws | c6g.metal (Neoverse N1 = Altra core) | 2.18 | spot allowed |
| `arm-smoke-metal` | aws | a1.metal | 0.41 | boot/build smoke only; old A72 cores |
| `intel-metal-aws` | aws | c5.metal | 4.08 | rare; Hetzner box is the Intel metal reference |
| `amd-epyc-metal` | aws | c6a.metal | 7.34 | STOPGAP — hourly-EPYC provider selection is a separate bead |
| `intel-pnap-cheap` | pnap | s0.d1.small | 0.08 | pnap bills whole hours — size leases accordingly |
| `misc-hetzner-vm` | hetzner | cx22 | 0.01 | utility box |

Banned (encode as refusals or omit): GCP E2 / any GCP AMD or ARM shape for nested
(unsupported); plain AWS VMs outside the supported-family list for nested profiles.

## Budget algorithm & ledger

`spend_today(UTC) = Σ ledger charges dated today + Σ pessimistic charges of currently
live leases`. `lease`/`extend` refuse when `spend_today + new_charge > cap` (exit 2).

**Ledger = a pinned GitHub issue in the tool repo.** The CLI posts one JSON comment per
`lease` / `extend` / `release` / sweep-kill event (append-only, no write races, queryable
via `gh api`, human-auditable in the UI). Nothing is ever recorded manually. If GitHub is
unreachable, `lease` fails closed (exit 6); `--no-ledger` exists for Paul only, prints a
loud warning, and must never be used by agents.

**Kill switch:** repo variable `HCV_ENABLED` in the tool repo, read via `gh api`.
`lease` refuses when false (exit 3). `sweep`/`scram`/`release` always work. `scram` sets
it false as its first action.

## Kill layers

| layer | gcp | aws | pnap | hetzner |
|---|---|---|---|---|
| L1 in-instance (best-effort) | scheduled `shutdown -h` at lease end — a stopped GCP instance stops compute billing; sweeper deletes later | launch with `--instance-initiated-shutdown-behavior terminate` + scheduled `shutdown -h` → self-terminates, **no credentials shipped** | none — no instance-scoped creds exist; whole-hour billing makes sweep latency free | none — hcloud tokens are project-wide, NEVER ship one to an instance |
| L2 sweeper (authoritative) | GHA cron in the tool repo, every 15 min: `hcv sweep` — delete past-lease managed resources on all providers, report any anomaly (managed tag but no lease tag, etc.) | same | same (owns pnap teardown) | same (owns hcloud teardown) |
| L3 backstop | provider billing alerts, set up manually by Paul (suggested: GCP $60/mo, AWS $150/mo, pnap $200/mo, hetzner $50/mo) | | | |

`scram`: phone-reachable via `workflow_dispatch` in the tool repo.

## Payloads

**Runner mode:** cloud-init fetches actions-runner, mints a registration token for the
target repo (`gh api repos/<owner>/<repo>/actions/runners/registration-token` — needs a
PAT stored as tool-repo secret `HCV_RUNNER_PAT`; local runs use Paul's `gh` auth),
registers `--ephemeral --labels <profile.runner_labels>`, runs one job, then powers off.
The lease TTL applies regardless — an idle runner that never gets a job still dies on
schedule.

**Dev mode:** inject the configured public key, print the `ssh` line, support
`hcv ssh <id>`. Dev leases get a warning ledger comment when <30 min remain (sweep adds
it); `hcv extend` is the response.

## Credentials model

Three tiers; each thing holds the least it can.

**Tier 0 — owner credentials (Paul only, never stored, never agent-readable).** Provider
root/console auth: `gcloud auth login`, an AWS admin profile, the pnap portal, the
Hetzner console. Used for exactly two things: running `hcv bootstrap <provider>` and
revoking Tier 1 credentials. Bootstrap consumes them from ambient CLI/portal auth and
writes nothing back to them.

**Tier 1 — operator credentials (what `hcv` runs as).** Created by `bootstrap`, one
principal per provider. Blast radius = tagged instances inside one dedicated
project/account surface:

| provider | principal | scoping wall | local (Mac) | GHA (tool-repo secrets) |
|---|---|---|---|---|
| gcp | `hcv-operator` service account in a **dedicated GCP project** | project isolation + minimal custom role (instance create/delete/get/list, subnetwork use; NO `iam.*`, NO storage, NO project Editor). GCP IAM can't condition deletes on labels, so the project boundary IS the wall | SA key activated as named gcloud config `hcv` | `GCP_SA_KEY` |
| aws | IAM user `hcv-operator` | **IAM tag conditions enforce the managed-tag rule at the provider**: `RunInstances` requires `aws:RequestTag/hcv-managed=true`; `TerminateInstances` requires `ec2:ResourceTag/hcv-managed=true`; `CreateTags` allowed only via `ec2:CreateAction=RunInstances`; single-region condition | access key under `AWS_PROFILE=hcv` | `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` |
| pnap | OAuth client (id+secret) from the BMC portal | account-level (weakest wall — acceptable: the pnap account holds nothing else) | `~/.config/hcv/pnap.toml` (0600) | `PNAP_CLIENT_ID` / `PNAP_CLIENT_SECRET` |
| hetzner | hcloud API token for a **dedicated, otherwise-empty Hetzner Cloud project** | project-scoped token; the hcloud API cannot reach Robot dedicated servers at all → determinism box invisible | `~/.config/hcv/hetzner.toml` (0600) | `HCLOUD_TOKEN` |
| github | **fine-grained PAT**, harmony repo ONLY, Administration read/write (mints runner registration tokens), 90-day expiry | single-repo fine-grained PAT | Paul's `gh` auth | `HCV_RUNNER_PAT` |

**`hcv` never uses default CLI profiles.** Every shell-out pins the operator identity
explicitly (`CLOUDSDK_ACTIVE_CONFIG_NAME=hcv`, `AWS_PROFILE=hcv`, `HCLOUD_TOKEN=…` from
its own config), so an agent driving `hcv` on Paul's Mac cannot wander into Paul's
personal default credentials — and raw provider CLIs stay deny-listed in agent
permission rules, making `hcv` the only path twice over.

**Tier 2 — instance-side: a leased box holds zero long-lived secrets.** AWS
self-termination is a launch attribute (no creds); GCP instances launch with NO service
account attached; pnap/hetzner teardown is entirely sweeper-side. The only secrets a box
ever sees: a single-use ~1-hour runner registration token (minted per lease via
`HCV_RUNNER_PAT`, dead once consumed) and a public SSH key.

**Rotation & compromise.** Every Tier 1 credential is disposable: revoke in the
provider console, re-run `hcv bootstrap` (idempotent), update the two storage spots.
Worst-case leak of the strongest one (AWS operator): the attacker can run *tagged*
instances in *one region* up to quota — no data access, no IAM escalation — caught by
the L3 billing alerts and killed by `scram`. v1 deliberately uses static scoped keys
(simple, revocable); upgrading GHA auth to keyless OIDC/WIF is an M6 nicety, not a v1
requirement.

## Safety invariants (hard; each needs a test)

1. One shared code path applies the `hcv-managed` filter for ALL destructive operations;
   unit tests prove `release`/`sweep`/`scram` refuse resources missing it.
2. No account-wide terminate/delete API is ever called. Hetzner Robot is read-only.
3. Tags atomically at creation (see Tag schema). No create-then-tag path exists.
4. All times UTC; lease-until as epoch seconds.
5. Budget charges pessimistic; ledger fail-closed.
6. `bootstrap` is idempotent and has `--check`; it is run by Paul with human-authed
   CLIs. **Agents never hold root/owner credentials** — bootstrap's output is the scoped
   credential set that agents and workflows use.
7. `sweep` is safe to run concurrently with itself and with `release`.

## Spend rules for the implementing agent (hard)

- Total live-test spend for this entire task: **≤ $5**. Record each live smoke's actual
  cost in the PR description.
- Default to mocks: a fake-CLI shim harness (fake `gcloud`/`aws`/`hcloud` on PATH
  emitting canned JSON; a local HTTP stub for pnap) covers all logic tests with zero
  spend.
- Live smokes ONLY at milestone gates, behind `HCV_LIVE_SMOKE=1`, fired once
  (smoke-fire-once discipline, per bd memory).
- End every working session at zero live resources: `hcv ls` must be empty; run
  `hcv sweep` and verify. Never leave anything running unattended.

## Milestones

**M0 — scaffold (no cloud calls).** Repo layout, clap CLI skeleton with all verbs,
profiles.toml schema + seed entries, provider driver trait, `--dry-run` for every
mutating verb, mock-shim test harness. Gate: `cargo test` green; golden `--dry-run`
output committed for every verb × provider.

**M1 — GCP driver + core machinery.** `lease/ls/release/sweep/budget` end-to-end on
gcp; ledger issue posting; kill-switch check; L1 payload. Live gate (~$0.15):
(a) `hcv lease nested-gcp-intel-smoke --hours 1 --spot`, verify `/dev/kvm` exists in
the L1 guest OS, `hcv release`; (b) lease with a 5-minute TTL, run `hcv sweep`, verify
it kills and posts the ledger event.

**M2 — AWS driver.** Including nested `--cpu-options` and metal handling
(`--instance-initiated-shutdown-behavior terminate`; metal provisioning takes 10–20 min —
surface that in `lease` output and lease sizing). Live gate (~$0.30): c7i.2xlarge nested
smoke — boot, verify `/dev/kvm`, release. Optionally ONE a1.metal smoke (~$0.25) if the
metal code path needs live proof. Do NOT live-test c6g/c5/c6a here — that spend belongs
to the programs that use them.

**M3 — phoenixNAP driver (REST).** Live gate (~$0.20): cheapest shape, one whole hour,
torn down by sweep. Confirm the a1.c5.large hourly price in the BMC portal, fill
profiles.toml, flag to Paul if > $2/hr.

**M4 — Hetzner driver.** hcloud full lifecycle against the dedicated project. No Robot
integration (see Credentials model). Live gate (~$0.02): cx22 lifecycle.

**M5 — control plane + runner proof.** GHA workflows in the tool repo: sweep cron
(15 min), `lease`/`scram` on workflow_dispatch; `HCV_ENABLED` wiring. Runner-mode proof:
register an ephemeral runner into harmony and run a trivial workflow there (the
harmony-side workflow file is a one-file PR — coordinate with Paul/foreman). Gate:
full loop lease→runner→job→self-terminate→consistent ledger; plus a **scram drill** —
two cheap leases on two providers, `hcv scram`, verify zero survivors on all providers
and `HCV_ENABLED=false`.

**M6 — later (do NOT start without dispatch):** nightly billing reconciliation
(computed vs provider billing data, alert on drift — catches untagged leaks and stale
rates), keyless GHA auth (OIDC/WIF), dev-mode QoL.

## Prerequisites (Paul, before M1's live gate)

- Create the GitHub repo; set `HCV_ENABLED=true`; create the pinned ledger issue.
- Accounts/credentials per the Credentials model: dedicated GCP project + billing; AWS
  account; pnap account + OAuth client; dedicated empty Hetzner Cloud project + token;
  fine-grained `HCV_RUNNER_PAT`. Run `hcv bootstrap <p>` per provider with owner creds
  (Tier 0); put the Tier 1 outputs in tool-repo secrets and `~/.config/hcv/`.
- Set L3 billing alerts (thresholds above).

## Non-goals

Terraform; Oracle Cloud (not used, ever); Hetzner Robot integration; instance
pooling/autoscaling; cost optimization beyond the cap; managing any pre-existing/
untagged machine (the determinism box is explicitly out of scope); Windows;
multi-user/team semantics.

## Open items

- pnap a1.c5.large price confirmation → M3.
- Hourly AMD EPYC bare-metal provider selection (Vultr / Scaleway / Latitude.sh /
  Hetzner auction) → separate bead; `amd-epyc-metal` (c6a.metal) is the stopgap.
- Binary name `hcv` — Paul may override at first review.
