// SPDX-License-Identifier: AGPL-3.0-or-later
//! # harmony-flow-agent — the in-guest flow agent brain (task 61)
//!
//! The first true guest-plane fault path, end to end: the host **decides** a
//! per-flow network policy (`net_decide`), and this agent — a static Linux binary
//! living in the workload initramfs — **enforces** it on the intra-guest CNI with
//! Linux's own mechanisms. It is the first real consumer of the `hypercall-doorbell`
//! doorbell.
//!
//! This crate is the agent's **brain**, decoupled from the syscalls it drives so
//! it builds and is unit-tested on the macOS dev host (the Linux command
//! *execution* lives in `main.rs`). Three seams:
//!
//! 1. [`policy_from_answer`] — maps the host's environment-encoded
//!    [`Answer`](environment::Answer) (a `Nominal`, or a `NetLatency`/`NetLoss`/
//!    `NetThrottle`/`NetReset` [`Fault`](environment::Fault)) onto `dissonance/flow`'s
//!    [`FlowPolicy`] vocabulary. The `flow` crate is embedded here as the policy
//!    vocabulary and the [`FlowDecider`] seam; its byte-stream `ToxiproxyEngine` is
//!    **not** used — see the divergence note below.
//! 2. [`HostFlowDecider`] — a [`FlowDecider`] that issues `net_decide` over a
//!    hypercall [`Client`](hypercall_proto::Client) and decodes the answer into a
//!    [`FlowPolicy`]. Asked **once per flow/connection** (the host is on the
//!    control path only).
//! 3. [`enforcement_commands`] — turns a [`FlowPolicy`] into the concrete,
//!    deterministic Linux enforcement commands (`tc netem`/`tbf`, `nftables`
//!    drop/reject) the agent runs on the flow's interface.
//!
//! ## Enforcement-determinism discipline
//!
//! Every input the agent acts on comes from a determinized source by
//! construction — the guest clock (V-time-backed), the host-answered policy — and
//! it has no other sources (consonance denies them). Concretely: a `netem delay`
//! is expressed in the guest's own time (deterministic under the determinized
//! kernel clock); a full drop / partition is a standing `nftables` verdict (no
//! RNG at all). The one policy that needs a *seeded* PRNG — **fractional**
//! `NetLoss` (`den > 1`) — is deliberately **not** enforceable by this prototype
//! (see below), because `tc netem loss` draws from the kernel's own unseeded PRNG,
//! which is exactly the non-determinism this project exists to eliminate.
//!
//! ## Divergence from task-51's abstractions (recorded per the spec)
//!
//! Task 51's design routes every flow through one central userspace L4 proxy
//! (iptables REDIRECT → `flow::ToxiproxyEngine`), which models delivery as a
//! byte-stream `Deliver`/`Reset` schedule and does seeded-PRNG fractional loss in
//! userspace. Per the integrator ruling for this first vertical, the agent instead
//! installs **in-kernel** enforcement (the "nftables-verdict prototype" the spec
//! permits): it asks `net_decide` once per flow and programs a `tc`/`nft` rule,
//! never splicing bytes in userspace. Consequences, so task-51's abstractions are
//! *corrected* rather than silently bypassed:
//!
//! - The [`ToxiproxyEngine`](flow::ToxiproxyEngine) byte-proxy is unused; only the
//!   [`FlowPolicy`] vocabulary and the [`FlowDecider`] seam are embedded.
//! - Fractional `NetLoss` (`den > 1`, `num < den`) needs the seeded-PRNG userspace
//!   proxy and is reported as [`EnfError::FractionalLossUnsupported`] — a follow-on
//!   builds the proxy shell. Full drop (`num >= den`, e.g. `1/1`) and standing
//!   partitions ARE enforced (a standing `nft drop`).

use environment::{Answer, Fault};
use flow::{ConnId, FlowDecider, FlowPolicy, NodeId, Span};
use hypercall_proto::{Client, ClientError, Status, Transport};

/// The fixed `nftables` table + chain the agent installs its verdict rules into.
/// A single named table keeps enforcement idempotent and easy to flush on exit.
pub const NFT_TABLE: &str = "harmony_flow";
/// The chain (in [`NFT_TABLE`]) holding the per-flow verdict rules.
pub const NFT_CHAIN: &str = "flowout";

/// Errors mapping a host [`Answer`] onto a [`FlowPolicy`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MapError {
    /// The answer was a [`Answer::Supply`] — a supply-class value, never valid for
    /// the fault-only `NetFlow` class. A well-formed host never sends this for a
    /// flow decision; the agent refuses it rather than guessing.
    SupplyForFlow,
    /// The answer carried a [`Fault`] of a non-`NetFlow` class (a block/process/
    /// buggify fault). The host mis-answered a flow decision; refuse it.
    WrongFaultClass,
}

impl core::fmt::Display for MapError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::SupplyForFlow => f.write_str("a Supply answer is not valid for a network flow"),
            Self::WrongFaultClass => f.write_str("the fault is not a NetFlow-class fault"),
        }
    }
}

impl std::error::Error for MapError {}

/// Map the host's per-flow [`Answer`] onto `flow`'s [`FlowPolicy`]. `seed` is the
/// per-connection seed a seeded-loss policy would draw from (derived by the caller
/// from the recorded flow identity, so replay is exact); it is only consulted for
/// [`Fault::NetLoss`]. A `Nominal` answer is a `Nominal` policy; a non-`NetFlow`
/// answer is a [`MapError`] (the agent never silently mis-enforces a mismatched
/// answer).
pub fn policy_from_answer(answer: &Answer, seed: u64) -> Result<FlowPolicy, MapError> {
    match answer {
        Answer::Nominal => Ok(FlowPolicy::Nominal),
        Answer::Supply(_) => Err(MapError::SupplyForFlow),
        Answer::Fault(fault) => match fault {
            Fault::NetLatency(d) => Ok(FlowPolicy::Latency(Span(d.0))),
            Fault::NetLoss { num, den } => Ok(FlowPolicy::Loss {
                seed,
                num: *num,
                den: *den,
            }),
            Fault::NetThrottle { bps } => Ok(FlowPolicy::Throttle { bps: *bps }),
            Fault::NetReset => Ok(FlowPolicy::Reset),
            // Any non-net fault is a class mismatch for a flow decision.
            _ => Err(MapError::WrongFaultClass),
        },
    }
}

/// Errors synthesizing an enforcement plan from a [`FlowPolicy`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnfError {
    /// A **fractional** [`FlowPolicy::Loss`] (`den > 1`, `num < den`) requires the
    /// seeded-PRNG userspace proxy (task 51's shell), which this in-kernel
    /// prototype deliberately defers: `tc netem loss` draws from the kernel's own
    /// unseeded PRNG and would be non-deterministic. Full drop (`num >= den`) is
    /// supported as a standing `nft drop`. Carries the offending `num/den`.
    FractionalLossUnsupported {
        /// Numerator of the unsupported drop fraction.
        num: u16,
        /// Denominator of the unsupported drop fraction.
        den: u16,
    },
}

impl core::fmt::Display for EnfError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::FractionalLossUnsupported { num, den } => write!(
                f,
                "fractional NetLoss {num}/{den} needs the deferred seeded-PRNG proxy \
                 (only full drop is enforced in-kernel)"
            ),
        }
    }
}

impl std::error::Error for EnfError {}

/// The concrete flow whose enforcement the agent programs, as a **structured
/// tuple** so both `nft` and `tc` can build a precise per-flow match (never shape
/// or drop more than the decided flow). Supplied by the init script that knows the
/// CNI layout — the bridge the pod→pod traffic is *forwarded* across plus the
/// server's pod IP and port — never derived from a nondeterministic source.
///
/// Intra-guest pod→pod traffic is **forwarded** across the CNI bridge, not locally
/// output, so the enforcement lands on the FORWARD path (`nft` `forward` hook, `tc`
/// on the bridge with a dst-matched filter), not `output`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlowTarget {
    /// The CNI bridge the pod→pod flow is forwarded across (e.g. `cni0`) — where
    /// the `tc` qdisc attaches.
    pub iface: String,
    /// The destination (server) pod IPv4 the flow addresses (e.g. `10.42.0.3`).
    pub dst_ip: String,
    /// The destination TCP port (e.g. `5432`).
    pub dport: u16,
}

/// One enforcement command the agent executes (`program` + `args`). Kept as data
/// (not run here) so the plan is a pure, unit-testable function of the policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnfCommand {
    /// The program to exec (`tc` or `nft`).
    pub program: String,
    /// Its argument vector.
    pub args: Vec<String>,
}

impl EnfCommand {
    fn new(program: &str, args: &[&str]) -> Self {
        Self {
            program: program.to_string(),
            args: args.iter().map(|a| (*a).to_string()).collect(),
        }
    }
}

/// One V-time unit maps to one microsecond of guest netem delay. The guest kernel
/// clock is V-time-backed under consonance, so a delay expressed in the guest's
/// own time is deterministic; the unit scale is a documented knob (identity ×1µs),
/// not a source of nondeterminism. `saturating` so a hostile `u64::MAX` delay
/// clamps rather than wraps.
fn vtime_to_micros(d: Span) -> u64 {
    d.0
}

/// Synthesize the deterministic Linux enforcement commands for `policy` on
/// `target`. `Nominal` yields an empty plan (nothing installed — the agent's mere
/// presence is inert). Ordering is fixed and total; the caller runs the commands
/// in order.
///
/// Every mechanism is **filtered to the decided flow** (`dst_ip:dport`), on the
/// **FORWARD path** the intra-guest pod→pod traffic actually traverses — not a
/// broad qdisc that would shape every flow on the bridge, and not `output` (which
/// bridged forward traffic never hits):
///
/// - [`FlowPolicy::Nominal`] → `[]`.
/// - [`FlowPolicy::Latency`] → a `tc prio` root + a `netem delay <µs>` child band +
///   a `u32` filter matching `dst_ip:dport` into that band (other flows: band 0,
///   unshaped).
/// - [`FlowPolicy::Throttle`] → the same filtered `tc` chain with a `tbf rate` band.
/// - [`FlowPolicy::Reset`] → an `nft` `forward`-hook rule rejecting the flow with a
///   TCP reset.
/// - [`FlowPolicy::Loss`] full drop (`num >= den`, or `den == 0` ⇒ no-op guard) →
///   an `nft` `forward`-hook `drop` rule; **fractional** loss →
///   [`EnfError::FractionalLossUnsupported`].
pub fn enforcement_commands(
    policy: &FlowPolicy,
    target: &FlowTarget,
) -> Result<Vec<EnfCommand>, EnfError> {
    match policy {
        FlowPolicy::Nominal => Ok(Vec::new()),
        FlowPolicy::Latency(d) => Ok(tc_filtered_qdisc(
            target,
            &["netem", "delay", &format!("{}us", vtime_to_micros(*d))],
        )),
        FlowPolicy::Throttle { bps } => Ok(tc_filtered_qdisc(
            target,
            &[
                "tbf",
                "rate",
                &format!("{bps}bps"),
                "burst",
                "1540",
                "latency",
                "50ms",
            ],
        )),
        FlowPolicy::Reset => Ok(nft_verdict(target, "reject with tcp reset")),
        FlowPolicy::Loss { num, den, .. } => {
            // `den == 0` is a deterministic no-op (deliver) by the catalog's
            // contract; `num >= den` is a full drop. Anything strictly fractional
            // needs the seeded-PRNG userspace proxy and is refused here.
            if *den == 0 {
                Ok(Vec::new())
            } else if num >= den {
                Ok(nft_verdict(target, "drop"))
            } else {
                Err(EnfError::FractionalLossUnsupported {
                    num: *num,
                    den: *den,
                })
            }
        }
    }
}

/// Build the `nft` command sequence installing a standing `<verdict>` rule matching
/// the decided flow on the **FORWARD hook** (intra-guest pod→pod traffic is
/// forwarded across the CNI bridge, never locally output; `br_netfilter` /
/// `bridge-nf-call-iptables=1`, set by the init script, makes the `inet` forward
/// hook see bridged frames). Idempotent table/chain creation keeps re-runs clean.
fn nft_verdict(target: &FlowTarget, verdict: &str) -> Vec<EnfCommand> {
    let rule = format!(
        "ip daddr {} tcp dport {} {}",
        target.dst_ip, target.dport, verdict
    );
    vec![
        EnfCommand::new("nft", &["add", "table", "inet", NFT_TABLE]),
        EnfCommand::new(
            "nft",
            &[
                "add",
                "chain",
                "inet",
                NFT_TABLE,
                NFT_CHAIN,
                "{ type filter hook forward priority 0 ; }",
            ],
        ),
        EnfCommand::new("nft", &["add", "rule", "inet", NFT_TABLE, NFT_CHAIN, &rule]),
    ]
}

/// Build a `tc` chain that shapes **only the decided flow**: a `prio` root qdisc,
/// the shaping qdisc (`netem`/`tbf`, given as `child_args`) as a child band, and a
/// `u32` filter classifying `dst_ip:dport` TCP packets into that band. Unmatched
/// flows fall to the default band and are delivered unshaped, so a broad "shape the
/// whole bridge" side effect never happens. Attached to the CNI bridge (`iface`),
/// on whose forwarded egress the pod→pod packets appear.
fn tc_filtered_qdisc(target: &FlowTarget, child_args: &[&str]) -> Vec<EnfCommand> {
    let dev = target.iface.as_str();
    let dport = target.dport.to_string();
    // prio root (handle 1:), a shaping child on band 1:3 (handle 30:), then a u32
    // filter matching dst ip + tcp dport into 1:3.
    let mut cmds = vec![
        EnfCommand::new(
            "tc",
            &["qdisc", "add", "dev", dev, "root", "handle", "1:", "prio"],
        ),
        EnfCommand::new(
            "tc",
            &[
                &["qdisc", "add", "dev", dev, "parent", "1:3", "handle", "30:"][..],
                child_args,
            ]
            .concat(),
        ),
    ];
    cmds.push(EnfCommand::new(
        "tc",
        &[
            "filter",
            "add",
            "dev",
            dev,
            "parent",
            "1:0",
            "protocol",
            "ip",
            "prio",
            "1",
            "u32",
            "match",
            "ip",
            "dst",
            &target.dst_ip,
            "match",
            "ip",
            "dport",
            &dport,
            "0xffff",
            "flowid",
            "1:3",
        ],
    ));
    cmds
}

/// A [`FlowDecider`] that resolves each flow by asking the host `net_decide` over
/// a hypercall [`Client`] and decoding the answer into a [`FlowPolicy`]. Asked
/// **once per flow** by an engine/driver; the host is on the control path only.
///
/// On any transport, protocol, decode, or class error it deterministically falls
/// back to [`FlowPolicy::Nominal`] (deliver normally) and records the failure in
/// [`last_error`](Self::last_error) — a guest-side transport fault must never make
/// enforcement diverge between two runs, and "deliver normally" is the safe,
/// nominal default. The `seed_fn` derives the per-connection loss seed from the
/// flow identity so a seeded-loss policy replays exactly.
pub struct HostFlowDecider<'a, T: Transport, F: FnMut(ConnId, NodeId, NodeId) -> u64> {
    client: &'a mut Client<T>,
    seed_fn: F,
    last_error: Option<DecideError>,
    decisions: Vec<(ConnId, FlowPolicy)>,
}

/// Why a [`HostFlowDecider`] fell back to `Nominal` for a flow.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecideError {
    /// The host has **no Net service wired** (an `UnknownService` status, or a
    /// `HostRejected`/transport outcome — the doorbell was not serviced). This is
    /// the *expected* case for a guest whose host did not `enable_net`: the agent
    /// no-ops cleanly (delivers normally, installs nothing), never errors or hangs.
    DoorbellUnwired,
    /// The `net_decide` hypercall failed for another reason (a non-`Ok` status
    /// other than `UnknownService`, a protocol/framing error).
    Hypercall(String),
    /// The answer bytes did not decode as an [`Answer`].
    Decode,
    /// The decoded answer was not a valid `NetFlow` policy (see [`MapError`]).
    Map(MapError),
}

impl<'a, T: Transport, F: FnMut(ConnId, NodeId, NodeId) -> u64> HostFlowDecider<'a, T, F> {
    /// Wrap a hypercall client and a per-connection seed deriver.
    pub fn new(client: &'a mut Client<T>, seed_fn: F) -> Self {
        Self {
            client,
            seed_fn,
            last_error: None,
            decisions: Vec::new(),
        }
    }

    /// The reason the most recent [`decide_flow`](FlowDecider::decide_flow) fell
    /// back to `Nominal`, if it did (cleared to `None` on a clean decision).
    pub fn last_error(&self) -> Option<&DecideError> {
        self.last_error.as_ref()
    }

    /// Every `(conn, policy)` this decider resolved, in ask order — the agent logs
    /// them to the serial console as the run's flow-decision evidence.
    pub fn decisions(&self) -> &[(ConnId, FlowPolicy)] {
        &self.decisions
    }
}

impl<T: Transport, F: FnMut(ConnId, NodeId, NodeId) -> u64> FlowDecider
    for HostFlowDecider<'_, T, F>
{
    fn decide_flow(&mut self, conn: ConnId, src: NodeId, dst: NodeId) -> FlowPolicy {
        let seed = (self.seed_fn)(conn, src, dst);
        let mut out = [0u8; 64];
        let policy = match self.client.net_decide(src.0, dst.0, conn.0, 0, &mut out) {
            Ok(n) => match Answer::decode(&out[..n]) {
                Ok(answer) => match policy_from_answer(&answer, seed) {
                    Ok(p) => {
                        self.last_error = None;
                        p
                    }
                    Err(e) => {
                        self.last_error = Some(DecideError::Map(e));
                        FlowPolicy::Nominal
                    }
                },
                Err(_) => {
                    self.last_error = Some(DecideError::Decode);
                    FlowPolicy::Nominal
                }
            },
            Err(e) => {
                self.last_error = Some(classify_client_error(&e));
                FlowPolicy::Nominal
            }
        };
        self.decisions.push((conn, policy.clone()));
        policy
    }
}

/// Classify a hypercall error: an `UnknownService` status or any transport-level
/// failure (`HostRejected` — the doorbell wasn't serviced) means the host has no
/// Net service wired, which the agent treats as a clean [`DecideError::DoorbellUnwired`]
/// no-op; anything else is a genuine [`DecideError::Hypercall`].
fn classify_client_error<E>(e: &ClientError<E>) -> DecideError {
    match e {
        ClientError::Transport(_) => DecideError::DoorbellUnwired,
        ClientError::Status(Status::UnknownService) => DecideError::DoorbellUnwired,
        ClientError::Protocol(p) => DecideError::Hypercall(format!("protocol: {p}")),
        ClientError::SeqMismatch => DecideError::Hypercall("seq-mismatch".to_string()),
        ClientError::Status(s) => DecideError::Hypercall(format!("status: {s:?}")),
        ClientError::InvalidLength => DecideError::Hypercall("invalid-length".to_string()),
    }
}

/// A startup self-check that two reads of a determinized source agree — cheap
/// insurance that the agent's inputs really are determinized before it enforces
/// anything. Returns `Ok(())` iff `first == second`; the Linux `main` supplies the
/// two samples (two `/dev/urandom` reads across the check, two clock reads,
/// timerfd behavior). Kept here as pure logic so the comparator is unit-tested.
pub fn selfcheck_agree<S: PartialEq>(label: &str, first: &S, second: &S) -> Result<(), String> {
    if first == second {
        Ok(())
    } else {
        Err(format!(
            "determinism self-check failed: two reads of '{label}' disagree"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use environment::Span as EnvSpan;

    fn target() -> FlowTarget {
        FlowTarget {
            iface: "cni0".to_string(),
            dst_ip: "10.42.0.3".to_string(),
            dport: 5432,
        }
    }

    #[test]
    fn nominal_answer_maps_to_nominal_policy_and_empty_plan() {
        let p = policy_from_answer(&Answer::Nominal, 0).unwrap();
        assert_eq!(p, FlowPolicy::Nominal);
        assert_eq!(enforcement_commands(&p, &target()).unwrap(), Vec::new());
    }

    #[test]
    fn net_faults_map_onto_flow_policy() {
        assert_eq!(
            policy_from_answer(&Answer::Fault(Fault::NetLatency(EnvSpan(500))), 0).unwrap(),
            FlowPolicy::Latency(Span(500))
        );
        assert_eq!(
            policy_from_answer(&Answer::Fault(Fault::NetThrottle { bps: 1000 }), 0).unwrap(),
            FlowPolicy::Throttle { bps: 1000 }
        );
        assert_eq!(
            policy_from_answer(&Answer::Fault(Fault::NetReset), 0).unwrap(),
            FlowPolicy::Reset
        );
        assert_eq!(
            policy_from_answer(&Answer::Fault(Fault::NetLoss { num: 1, den: 1 }), 77).unwrap(),
            FlowPolicy::Loss {
                seed: 77,
                num: 1,
                den: 1
            }
        );
    }

    #[test]
    fn non_net_answers_are_refused() {
        assert_eq!(
            policy_from_answer(&Answer::Supply(vec![1, 2, 3]), 0),
            Err(MapError::SupplyForFlow)
        );
        assert_eq!(
            policy_from_answer(&Answer::Fault(Fault::BlockEio), 0),
            Err(MapError::WrongFaultClass)
        );
    }

    #[test]
    fn latency_plan_is_a_flow_filtered_netem_delay() {
        let cmds = enforcement_commands(&FlowPolicy::Latency(Span(2500)), &target()).unwrap();
        // prio root, netem child on band 1:3, and a u32 filter to the flow.
        assert_eq!(cmds.len(), 3);
        assert!(cmds.iter().all(|c| c.program == "tc"));
        assert!(cmds[0].args.contains(&"prio".to_string()));
        assert_eq!(cmds[1].args.last().unwrap(), "2500us");
        assert!(cmds[1].args.contains(&"netem".to_string()));
        // The classifier filters to the decided flow, not the whole bridge.
        let filt = &cmds[2].args;
        assert_eq!(filt[0], "filter");
        assert!(filt.windows(2).any(|w| w == ["dst", "10.42.0.3"]));
        assert!(filt.contains(&"dport".to_string()) && filt.contains(&"5432".to_string()));
        assert_eq!(filt.last().unwrap(), "1:3");
    }

    #[test]
    fn throttle_plan_is_a_flow_filtered_tbf_rate() {
        let cmds = enforcement_commands(&FlowPolicy::Throttle { bps: 4096 }, &target()).unwrap();
        assert_eq!(cmds.len(), 3);
        assert!(cmds[1].args.contains(&"tbf".to_string()));
        assert!(cmds[1].args.contains(&"4096bps".to_string()));
        assert!(cmds[2].args.windows(2).any(|w| w == ["dst", "10.42.0.3"]));
    }

    #[test]
    fn full_drop_and_reset_are_flow_matched_forward_nft_verdicts() {
        let drop = enforcement_commands(
            &FlowPolicy::Loss {
                seed: 0,
                num: 1,
                den: 1,
            },
            &target(),
        )
        .unwrap();
        assert!(drop.iter().all(|c| c.program == "nft"));
        // The chain hooks FORWARD (not output — bridged pod→pod is forwarded).
        assert!(
            drop.iter()
                .any(|c| c.args.iter().any(|a| a.contains("hook forward")))
        );
        // The rule matches the specific flow and drops it.
        let rule = drop.last().unwrap().args.last().unwrap();
        assert!(rule.contains("ip daddr 10.42.0.3") && rule.contains("tcp dport 5432"));
        assert!(rule.ends_with("drop"));

        let reset = enforcement_commands(&FlowPolicy::Reset, &target()).unwrap();
        assert!(
            reset
                .last()
                .unwrap()
                .args
                .last()
                .unwrap()
                .contains("reject with tcp reset")
        );
    }

    #[test]
    fn fractional_loss_is_refused_not_misenforced() {
        assert_eq!(
            enforcement_commands(
                &FlowPolicy::Loss {
                    seed: 0,
                    num: 1,
                    den: 3
                },
                &target()
            ),
            Err(EnfError::FractionalLossUnsupported { num: 1, den: 3 })
        );
        // A `den == 0` fraction is a deterministic no-op, not an error.
        assert_eq!(
            enforcement_commands(
                &FlowPolicy::Loss {
                    seed: 0,
                    num: 5,
                    den: 0
                },
                &target()
            ),
            Ok(Vec::new())
        );
    }

    #[test]
    fn selfcheck_agrees_and_disagrees() {
        assert!(selfcheck_agree("urandom", &[1u8, 2, 3], &[1u8, 2, 3]).is_ok());
        assert!(selfcheck_agree("clock", &10u64, &11u64).is_err());
    }
}
