# Consonance workload-coupling audit

SPDX-License-Identifier: AGPL-3.0-or-later

Task 43 / Beads `hm-ciz`, audited 2026-07-18. The test is strict: consonance may
model a machine, an architecture, and its deterministic devices, but it must not
need to know which operating system, application, or acceptance workload runs on
that machine. Host-OS `cfg(target_os = "linux")` and references to the upstream
Linux KVM ABI are not guest-workload coupling.

| ID | Location | Classification | Behavior-neutral to fix? | Action |
|---|---|---|---|---|
| F1 | `consonance/vmm-core/src/vendor/x86/linux_loader.rs`; `vendor/x86/bringup.rs`; `vendor/x86/mod.rs` | **substrate-violation** — parses bzImage/setup-header bytes and constructs Linux `boot_params`, initramfs placement, page tables, GDT, and Linux entry state | No. Moving the seam must preserve exact initial memory/register bytes and every replay hash | Deferred to `tasks/44-load-image-primitive.md`, expanded below to cover all OS presentation adapters |
| F2 | `consonance/vmm-core/tests/live_linux_boot.rs`, `live_postgres*.rs`, `live_k3s_postgres.rs`, `live_runc_postgres.rs`, `live_sdk.rs`, and the other `live_*` image gates | **consumer-coupling** — these are explicit end-to-end consumers of the generic VMM | Yes for path relocation, but relocation adds no correctness by itself | Leave in place as integration tests. They may name their workload; production substrate code may not derive behavior from it |
| F3 | `consonance/acceptance-suite/src/manifest.rs`, `corpus-manifest.example.toml`, and oracle tests | **consumer-coupling, acceptable** — the acceptance product intentionally defaults to the repository's acceptance payloads and goldens | Yes | Fixed inline by moving those assets under `consonance/acceptance-suite/{payloads,golden}` and repathing defaults |
| F4 | `consonance/vmm-core/src/corpus.rs`, `vendor/x86/dispatch.rs`, and related documentation using “corpus” or “Linux path” | **naming** where it describes an adapter/test composition; platform devices themselves are machine models, not Linux behavior | Yes | Keep “corpus” only for the glossary-defined acceptance workload suite. Treat “Linux path” as a composition label, never a device-policy condition |
| F5 | `consonance/hypercall-proto`, `hypercall-doorbell`, and `vmm-core` Event capture | **OS-agnostic ABI** — fixed pages, framed services, byte payloads, and deterministic dispatch contain no Linux or application format. Event id 0 remains opaque bytes to consonance | N/A | Leave. `/dev/harmony` and Antithesis JSON attribution live in `harmony-linux`; JSON decoding lives in `dissonance/sdk-events` |
| F6 | `consonance/vmm-core/src/vendor/arm64/image_loader.rs`, `vendor/arm64/entry.rs`, and `vendor/arm64/bringup.rs` | **substrate-violation** — the ARM64 vendor skeleton parses the Linux `Image` header and bakes the Linux/arm64 x0=DTB entry convention into substrate composition | No, for the same initial-state compatibility reason as F1 | Fold into task 44: harmony-linux produces opaque segments plus generic architecture entry state for both x86 and arm64 |
| F7 | `consonance/vmm-core/src/exec.rs` | **substrate-violation** — the deterministic engine knows BusyBox shell echoing, `$?`, and a workload-specific serial sentinel protocol | Yes at the wire level; public API relocation needs coordination | Track a follow-up Beads issue to move the shell protocol above consonance while retaining generic serial injection/capture below |
| F8 | `consonance/vmm-core/src/seal_rate.rs` (`WalFsync`, Postgres-oriented sampling vocabulary) and `src/corpus.rs` (`CorpusMachine`) | **consumer-coupling** — pure evaluation/frontier adapters are compiled into the substrate crate and name particular consumers | Yes, but it is packaging work rather than a task-43 path move | Track with F7; move evaluation adapters to acceptance/dissonance ownership on their next behavioral touch |

## Result

The live machine loop, backend contracts, snapshot state, device models, work clock,
and hypercall framing do not branch on a guest workload. The remaining production
violations are presentation/adaptation code at the edges of `vmm-core`, not hidden
policy in the run loop. Task 44 owns the boot-format pair (F1/F6); the recorded
follow-up owns shell/evaluation packaging (F7/F8). Task 43 intentionally does not
move either behavioral seam while performing the repository restructure.
