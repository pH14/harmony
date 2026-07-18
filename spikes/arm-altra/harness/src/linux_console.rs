// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded PL011 service loop for the AA-5(c) Linux boot path.
//!
//! The bare payload loop treats UART configuration accesses as evidence events.
//! Linux instead uses the full PrimeCell driver: it programs configuration
//! registers, reads the flag and PrimeCell ID registers, and writes console bytes
//! through `DR`. This module supplies precisely that userspace device seam and
//! stops at the marker the owned `/init` is specified to print. A console marker
//! alone cannot prove which component printed it, so this does not certify
//! userspace or determinism; it only makes a bounded boot observable.

use oracle_model::UART_BASE;
use thiserror::Error;

#[cfg(test)]
use crate::linux_boot::PVCLOCK_GPA;
use crate::linux_boot::{PVCLOCK_REGISTER_BASE, PVCLOCK_REGISTER_SIZE};
use crate::run::{
    ExactPreemptOutcome, RunError, StepVcpu, Vcpu, VcpuExit, WorkCounter, exact_arm_delta,
    service_exact_preempt,
};
use vtime::{VClock, VClockConfig};

const PL011_PAGE: u64 = 0x1000;
const PL011_DR_OFFSET: u64 = 0x000;
const PL011_FR_OFFSET: u64 = 0x018;
const PL011_FR_TXFE_RXFE: u64 = (1 << 7) | (1 << 4);
const PVCLOCK_REGISTER_GPA_OFFSET: u64 = 0;
const PVCLOCK_REGISTER_ABI_OFFSET: u64 = 8;
const PVCLOCK_REGISTER_END: u64 = PVCLOCK_REGISTER_BASE + PVCLOCK_REGISTER_SIZE;
/// Marker the owned AA-5 initramfs prints after `/init` reaches userspace.
pub const LINUX_READY_MARKER: &[u8] = b"HARMONY_AA5_READY";
/// Hard operational ceiling above the ordinary command default.
pub const MAX_CONSOLE_BYTES: usize = 64 << 20;
/// Hard operational exit ceiling above the ordinary command default.
pub const MAX_KVM_EXITS: u64 = 100_000_000;
const MAX_MARKER_BYTES: usize = 4096;
/// Default §2/G3 pvclock staleness bound, in retired branches (10 ms of contract V-time).
pub const DEFAULT_REFRESH_DELTA_WORK: u64 = 10_000_000;
/// Largest cadence the owned kernel's fixed 2^28-iteration registration spin can await.
pub const MAX_REFRESH_DELTA_WORK: u64 = 100_000_000;
const MAX_CLOCK_ADVISORY_EXITS: u64 = 100_000;

/// Limits and requested console marker for one Linux boot.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LinuxConsoleConfig {
    /// Exact byte sequence the owned `/init` is specified to print.
    pub ready_marker: Vec<u8>,
    /// Maximum KVM exits serviced before the boot is refused.
    pub max_exits: u64,
    /// Maximum captured console bytes.
    pub max_console_bytes: usize,
}

/// Bounded transcript produced after the requested marker was observed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LinuxConsoleResult {
    /// Console bytes through and including the first observed marker.
    pub console: Vec<u8>,
    /// Number of KVM exits serviced.
    pub exits: u64,
}

/// Exact-count work-clock configuration for the AA-5 Linux executor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LinuxWorkClockConfig {
    /// Maximum retired branches between distinct page publications.
    pub refresh_delta_work: u64,
    /// Measured N1 PMU skid margin used by arm-early + single-step landing.
    pub skid_margin: u64,
    /// Guest `CNTFRQ_EL0`, which the owned kernel cross-checks against the page.
    pub guest_clock_hz: u64,
}

/// Linux boot result plus the non-vacuous work-clock refresh evidence observed in-process.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LinuxWorkClockResult {
    /// Console transcript and total `KVM_RUN` count.
    pub boot: LinuxConsoleResult,
    /// Number of distinct post-registration page publications.
    pub publications: u64,
    /// Largest exact-work gap, including the cadence interval containing registration.
    pub max_refresh_gap_work: u64,
    /// Exact work count carried by the last published value.
    pub last_refresh_work: u64,
    /// Guest-selected page pinned by the one-shot ARM registration write.
    pub registration_gpa: u64,
}

/// Which ABI-v1 write the stopped-vCPU page seam must perform.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PvclockWrite {
    /// Whole-page registration form: sequence zero and reserved tail zeroed.
    Canonical,
    /// Value-keyed live refresh: sequence advances only if published values change.
    Refresh,
}

/// Resolve an untrusted guest-selected pvclock GPA to a bounded RAM slice.
#[cfg(any(test, target_os = "linux"))]
pub(crate) fn linux_pvclock_page_range(
    gpa: u64,
    ram_base: u64,
    ram_len: usize,
) -> Result<core::ops::Range<usize>, RunError> {
    if !gpa.is_multiple_of(vtime::pvclock::PVCLOCK_PAGE_LEN as u64) {
        return Err(RunError::Seam {
            context: "validate the guest-selected Linux pvclock page",
            message: format!("pvclock GPA {gpa:#x} is not page-aligned"),
        });
    }
    let page_offset = gpa
        .checked_sub(ram_base)
        .and_then(|offset| usize::try_from(offset).ok())
        .ok_or_else(|| RunError::Seam {
            context: "validate the guest-selected Linux pvclock page",
            message: format!(
                "pvclock GPA {gpa:#x} is below or not representable relative to RAM base {ram_base:#x}"
            ),
        })?;
    let page_end = page_offset
        .checked_add(vtime::pvclock::PVCLOCK_PAGE_LEN)
        .ok_or_else(|| RunError::Seam {
            context: "validate the guest-selected Linux pvclock page",
            message: "pvclock page end overflows usize".into(),
        })?;
    if page_end > ram_len {
        return Err(RunError::Seam {
            context: "validate the guest-selected Linux pvclock page",
            message: format!(
                "pvclock page [{page_offset:#x}, {page_end:#x}) is outside {ram_len} bytes of guest RAM"
            ),
        });
    }
    Ok(page_offset..page_end)
}

/// A step-capable Linux vCPU whose stopped guest RAM contains the pvclock page.
pub trait LinuxPvclockVcpu: StepVcpu {
    /// The page GPA already pinned in VM-owned state, if registration has succeeded.
    fn linux_pvclock_gpa(&self) -> Option<u64>;

    /// Validate and atomically pin an untrusted guest-selected clock-page GPA in VM state.
    ///
    /// # Errors
    /// [`RunError`] unless the complete aligned page lies in ordinary guest RAM.
    fn register_linux_pvclock_gpa(&mut self, gpa: u64) -> Result<(), RunError>;

    /// Publish one page value while the vCPU is stopped.
    ///
    /// # Errors
    /// [`RunError`] if the page no longer lies wholly inside guest RAM or read-back differs.
    fn publish_linux_pvclock(
        &mut self,
        vns: u64,
        guest_clock: u64,
        guest_clock_hz: u64,
        write: PvclockWrite,
    ) -> Result<(), RunError>;
}

/// Why a Linux console boot was refused.
#[derive(Debug, Error)]
pub enum LinuxConsoleError {
    /// The KVM seam failed.
    #[error(transparent)]
    Run(#[from] RunError),
    /// An empty marker would pass before proving userspace ran.
    #[error("the Linux ready marker is empty")]
    EmptyReadyMarker,
    /// A zero exit budget cannot observe a boot.
    #[error("the Linux KVM-exit budget is zero")]
    ZeroExitBudget,
    /// The marker cannot fit in the transcript budget.
    #[error("ready marker is {marker_len} bytes but console budget is {limit} bytes")]
    MarkerExceedsConsoleLimit {
        /// Marker length.
        marker_len: usize,
        /// Transcript limit.
        limit: usize,
    },
    /// The marker is too large for the bounded streaming matcher.
    #[error("ready marker length {requested} exceeds hard maximum {maximum}")]
    MarkerTooLarge {
        /// Requested marker length.
        requested: usize,
        /// Hard marker ceiling.
        maximum: usize,
    },
    /// Caller-supplied transcript bound exceeds the operational hard ceiling.
    #[error("console bound {requested} exceeds hard maximum {maximum}")]
    ConsoleBoundTooLarge {
        /// Requested byte bound.
        requested: usize,
        /// Hard byte ceiling.
        maximum: usize,
    },
    /// Caller-supplied exit bound exceeds the operational hard ceiling.
    #[error("exit bound {requested} exceeds hard maximum {maximum}")]
    ExitBoundTooLarge {
        /// Requested exit bound.
        requested: u64,
        /// Hard exit ceiling.
        maximum: u64,
    },
    /// KVM reported a zero-width MMIO access.
    #[error("KVM reported a zero-width PL011 MMIO access at {addr:#x}")]
    ZeroWidthMmio {
        /// Guest-physical MMIO address.
        addr: u64,
    },
    /// The userspace PL011 model supports byte/halfword/word accesses only.
    #[error("unsupported {width}-byte PL011 MMIO access at {addr:#x}")]
    UnsupportedMmioWidth {
        /// Guest-physical MMIO address.
        addr: u64,
        /// Reported byte width.
        width: usize,
    },
    /// The console did not produce the marker within the bounded transcript.
    #[error("Linux console exceeded its {limit}-byte bound before the ready marker")]
    ConsoleLimit {
        /// Transcript limit.
        limit: usize,
    },
    /// The guest did not produce the marker within the exit budget.
    #[error("Linux boot exceeded its {limit}-exit bound before the ready marker")]
    ExitLimit {
        /// Exit limit.
        limit: u64,
    },
    /// A measurement/debug mechanism exit has no place in an unarmed boot.
    #[error("unexpected {0} while booting Linux without an armed measurement")]
    UnexpectedMechanism(&'static str),
    /// A zero Δ cannot advance a materialized page and would arm an immediate-exit loop.
    #[error("the Linux work-clock refresh delta is zero")]
    ZeroRefreshDelta,
    /// The owned kernel's deterministic registration spin has a fixed bound, so the host must
    /// promise a first exact target comfortably inside it.
    #[error("Linux work-clock refresh delta {requested} exceeds the owned guest maximum {maximum}")]
    RefreshDeltaTooLarge {
        /// Requested retired-branch cadence.
        requested: u64,
        /// Largest cadence supported by the guest build.
        maximum: u64,
    },
    /// Restarting the executor would reset its VClock/cadence while retaining a live page.
    #[error("the Linux pvclock executor is single-use; this VM is already registered at {gpa:#x}")]
    PreexistingPvclockRegistration {
        /// VM-owned pinned page.
        gpa: u64,
    },
    /// The page frequency is contractual and must match a non-zero guest `CNTFRQ_EL0`.
    #[error("the Linux work-clock guest frequency is zero")]
    ZeroGuestClockHz,
    /// Guest work must start from the canonical reset anchor; pre-entry work means the perf
    /// event included execution outside the owned guest interval.
    #[error("the guest-only work counter read {work} before the first KVM entry; expected 0")]
    NonzeroInitialWork {
        /// Unexpected pre-entry work value.
        work: u64,
    },
    /// Advancing the periodic forced-refresh target would wrap and make time go backwards.
    #[error("the next Linux work-clock refresh target overflows after work {work}")]
    RefreshTargetOverflow {
        /// Last exact refresh work count.
        work: u64,
    },
    /// The ARM registration register is a one-shot for the VM's life. Any second write is a
    /// guest protocol fault even when it repeats the first GPA.
    #[error(
        "the guest attempted to re-register the Linux pvclock page at {attempted:#x}; the one-shot is already pinned to {registered:#x}"
    )]
    PvclockReregister {
        /// GPA already accepted.
        registered: u64,
        /// GPA carried by the rejected second write.
        attempted: u64,
    },
    /// The registration device exposes exactly one 64-bit write and one 32-bit read.
    #[error(
        "invalid Linux pvclock registration MMIO at {addr:#x}: width={width}, write={is_write}"
    )]
    InvalidPvclockRegisterMmio {
        /// Guest physical address KVM reported.
        addr: u64,
        /// Access width KVM reported.
        width: usize,
        /// Access direction KVM reported.
        is_write: bool,
    },
}

fn pl011_offset(addr: u64, width: usize) -> Result<u64, RunError> {
    let offset = addr
        .checked_sub(UART_BASE)
        .ok_or(RunError::UnexpectedMmio { addr })?;
    let width = u64::try_from(width).map_err(|_| RunError::UnexpectedMmio { addr })?;
    let end = offset
        .checked_add(width)
        .ok_or(RunError::UnexpectedMmio { addr })?;
    if offset >= PL011_PAGE || end > PL011_PAGE {
        return Err(RunError::UnexpectedMmio { addr });
    }
    Ok(offset)
}

fn read_value(offset: u64) -> u64 {
    match offset {
        PL011_FR_OFFSET => PL011_FR_TXFE_RXFE,
        // ARM PrimeCell PL011 peripheral and component IDs. Linux's AMBA probe
        // reads these before binding the full ttyAMA console driver.
        0xfe0 => 0x11,
        0xfe4 => 0x10,
        0xfe8 => 0x14,
        0xfec => 0x00,
        0xff0 => 0x0d,
        0xff4 => 0xf0,
        0xff8 => 0x05,
        0xffc => 0xb1,
        // No input, interrupts, or errors are pending. Writes to the model are
        // configuration-only; it is otherwise deliberately stateless.
        _ => 0,
    }
}

fn validate_config(config: &LinuxConsoleConfig) -> Result<(), LinuxConsoleError> {
    if config.ready_marker.is_empty() {
        return Err(LinuxConsoleError::EmptyReadyMarker);
    }
    if config.max_exits == 0 {
        return Err(LinuxConsoleError::ZeroExitBudget);
    }
    if config.max_exits > MAX_KVM_EXITS {
        return Err(LinuxConsoleError::ExitBoundTooLarge {
            requested: config.max_exits,
            maximum: MAX_KVM_EXITS,
        });
    }
    if config.max_console_bytes > MAX_CONSOLE_BYTES {
        return Err(LinuxConsoleError::ConsoleBoundTooLarge {
            requested: config.max_console_bytes,
            maximum: MAX_CONSOLE_BYTES,
        });
    }
    if config.ready_marker.len() > MAX_MARKER_BYTES {
        return Err(LinuxConsoleError::MarkerTooLarge {
            requested: config.ready_marker.len(),
            maximum: MAX_MARKER_BYTES,
        });
    }
    if config.ready_marker.len() > config.max_console_bytes {
        return Err(LinuxConsoleError::MarkerExceedsConsoleLimit {
            marker_len: config.ready_marker.len(),
            limit: config.max_console_bytes,
        });
    }
    Ok(())
}

/// Streaming Knuth-Morris-Pratt matcher: one amortized-constant update per
/// console byte, so a long repeated near-match cannot turn bounded input into
/// quadratic work.
struct MarkerMatcher {
    needle: Vec<u8>,
    prefix: Vec<usize>,
    matched: usize,
}

struct LinuxConsoleCapture {
    console: Vec<u8>,
    marker: MarkerMatcher,
    ready: bool,
}

impl LinuxConsoleCapture {
    fn new(config: &LinuxConsoleConfig) -> Self {
        Self {
            console: Vec::new(),
            marker: MarkerMatcher::new(&config.ready_marker),
            ready: false,
        }
    }

    fn service_mmio(
        &mut self,
        vcpu: &mut impl Vcpu,
        config: &LinuxConsoleConfig,
        addr: u64,
        data: &[u8],
        is_write: bool,
    ) -> Result<(), LinuxConsoleError> {
        if data.is_empty() {
            return Err(LinuxConsoleError::ZeroWidthMmio { addr });
        }
        if !matches!(data.len(), 1 | 2 | 4) {
            return Err(LinuxConsoleError::UnsupportedMmioWidth {
                addr,
                width: data.len(),
            });
        }
        let offset = pl011_offset(addr, data.len())?;
        if is_write {
            if offset != PL011_DR_OFFSET || self.ready {
                return Ok(());
            }
            if self.console.len() == config.max_console_bytes {
                return Err(LinuxConsoleError::ConsoleLimit {
                    limit: config.max_console_bytes,
                });
            }
            self.console.push(data[0]);
            self.ready = self.marker.push(data[0]);
        } else {
            let bytes = read_value(offset).to_le_bytes();
            vcpu.complete_mmio_read(&bytes[..data.len()])?;
        }
        Ok(())
    }
}

struct LinuxWorkMmio {
    console: LinuxConsoleCapture,
}

impl LinuxWorkMmio {
    fn new(config: &LinuxConsoleConfig) -> Self {
        Self {
            console: LinuxConsoleCapture::new(config),
        }
    }

    fn service(
        &mut self,
        vcpu: &mut impl LinuxPvclockVcpu,
        config: &LinuxConsoleConfig,
        addr: u64,
        data: &[u8],
        is_write: bool,
    ) -> Result<bool, LinuxConsoleError> {
        if !(PVCLOCK_REGISTER_BASE..PVCLOCK_REGISTER_END).contains(&addr) {
            self.console
                .service_mmio(vcpu, config, addr, data, is_write)?;
            return Ok(false);
        }

        let width = u64::try_from(data.len()).map_err(|_| {
            LinuxConsoleError::InvalidPvclockRegisterMmio {
                addr,
                width: data.len(),
                is_write,
            }
        })?;
        let access_end =
            addr.checked_add(width)
                .ok_or(LinuxConsoleError::InvalidPvclockRegisterMmio {
                    addr,
                    width: data.len(),
                    is_write,
                })?;
        let offset = addr - PVCLOCK_REGISTER_BASE;
        if access_end > PVCLOCK_REGISTER_END {
            return Err(LinuxConsoleError::InvalidPvclockRegisterMmio {
                addr,
                width: data.len(),
                is_write,
            });
        }

        match (offset, data.len(), is_write) {
            (PVCLOCK_REGISTER_GPA_OFFSET, 8, true) => {
                let attempted = u64::from_le_bytes(data.try_into().map_err(|_| {
                    LinuxConsoleError::InvalidPvclockRegisterMmio {
                        addr,
                        width: data.len(),
                        is_write,
                    }
                })?);
                if let Some(registered) = vcpu.linux_pvclock_gpa() {
                    return Err(LinuxConsoleError::PvclockReregister {
                        registered,
                        attempted,
                    });
                }
                // Validation precedes mutation: a rejected GPA does not consume the one-shot.
                vcpu.register_linux_pvclock_gpa(attempted)?;
                Ok(true)
            }
            (PVCLOCK_REGISTER_ABI_OFFSET, 4, false) => {
                let abi = if vcpu.linux_pvclock_gpa().is_some() {
                    vtime::pvclock::PVCLOCK_ABI_VERSION
                } else {
                    0
                };
                vcpu.complete_mmio_read(&abi.to_le_bytes())?;
                Ok(false)
            }
            _ => Err(LinuxConsoleError::InvalidPvclockRegisterMmio {
                addr,
                width: data.len(),
                is_write,
            }),
        }
    }
}

impl MarkerMatcher {
    fn new(needle: &[u8]) -> Self {
        let mut prefix = vec![0; needle.len()];
        for i in 1..needle.len() {
            let mut previous = prefix[i - 1];
            while previous > 0 && needle[i] != needle[previous] {
                previous = prefix[previous - 1];
            }
            if needle[i] == needle[previous] {
                previous += 1;
            }
            prefix[i] = previous;
        }
        Self {
            needle: needle.to_vec(),
            prefix,
            matched: 0,
        }
    }

    fn push(&mut self, byte: u8) -> bool {
        if self.needle.is_empty() {
            return true;
        }
        if self.matched == self.needle.len() {
            self.matched = self.prefix[self.matched - 1];
        }
        while self.matched > 0 && byte != self.needle[self.matched] {
            self.matched = self.prefix[self.matched - 1];
        }
        if byte == self.needle[self.matched] {
            self.matched += 1;
        }
        self.matched == self.needle.len()
    }
}

/// Run a Linux vCPU until its requested marker reaches PL011.
///
/// Configuration writes are accepted and ignored, `DR` writes emit one byte,
/// and reads return the fixed empty/ready PL011 state plus PrimeCell IDs. Every
/// exit and byte is bounded, and non-PL011 MMIO is refused.
///
/// # Errors
/// [`LinuxConsoleError`] if limits/configuration are invalid, the KVM seam fails,
/// or the guest produces an unexpected exit/access.
pub fn run_until_ready(
    vcpu: &mut impl Vcpu,
    config: &LinuxConsoleConfig,
) -> Result<LinuxConsoleResult, LinuxConsoleError> {
    validate_config(config)?;

    let mut capture = LinuxConsoleCapture::new(config);
    for exits in 1..=config.max_exits {
        match vcpu.run()? {
            VcpuExit::Mmio {
                addr,
                data,
                is_write,
            } => {
                capture.service_mmio(vcpu, config, addr, &data, is_write)?;
                if capture.ready {
                    return Ok(LinuxConsoleResult {
                        console: capture.console,
                        exits,
                    });
                }
            }
            VcpuExit::MalformedMmio { addr, width } => {
                return Err(RunError::MalformedMmio { addr, width }.into());
            }
            VcpuExit::Other(reason) => return Err(RunError::UnexpectedExit(reason).into()),
            VcpuExit::Preempt => {
                return Err(LinuxConsoleError::UnexpectedMechanism("KVM_EXIT_PREEMPT"));
            }
            VcpuExit::SignalKick => {
                return Err(LinuxConsoleError::UnexpectedMechanism("signal kick"));
            }
            VcpuExit::Debug => {
                return Err(LinuxConsoleError::UnexpectedMechanism("KVM_EXIT_DEBUG"));
            }
        }
    }
    Err(LinuxConsoleError::ExitLimit {
        limit: config.max_exits,
    })
}

/// Run Linux with a real ABI-v1 work-derived page refreshed at exact retired-branch Moments.
///
/// The host begins with no page target. The guest publishes one through the ARM one-shot MMIO
/// registration surface; the first exact forced landing canonically stamps that validated GPA,
/// and later landings refresh it. A patched in-kernel Preempt is armed early for each
/// `refresh_delta_work` target and the vCPU is single-stepped to the target's canonical PC before
/// publication. Natural exits do not import their skid-tainted live counter value. This proves
/// the registration/page/G3 half of AA-5; it deliberately does **not** claim the stock KVM
/// generic timer is deterministic (that independent timer-domain gap remains a live-boot blocker
/// in `docs/ARM-ALTRA.md`).
///
/// # Errors
/// [`LinuxConsoleError`] on invalid limits/clock configuration, a nonzero pre-entry work count,
/// any imprecise or unbounded landing, page publication failure, or an unexpected KVM exit.
pub fn run_until_ready_work_clock(
    vcpu: &mut impl LinuxPvclockVcpu,
    counter: &mut impl WorkCounter,
    console_config: &LinuxConsoleConfig,
    clock_config: LinuxWorkClockConfig,
) -> Result<LinuxWorkClockResult, LinuxConsoleError> {
    validate_config(console_config)?;
    if clock_config.refresh_delta_work == 0 {
        return Err(LinuxConsoleError::ZeroRefreshDelta);
    }
    if clock_config.refresh_delta_work > MAX_REFRESH_DELTA_WORK {
        return Err(LinuxConsoleError::RefreshDeltaTooLarge {
            requested: clock_config.refresh_delta_work,
            maximum: MAX_REFRESH_DELTA_WORK,
        });
    }
    if clock_config.guest_clock_hz == 0 {
        return Err(LinuxConsoleError::ZeroGuestClockHz);
    }
    if let Some(gpa) = vcpu.linux_pvclock_gpa() {
        return Err(LinuxConsoleError::PreexistingPvclockRegistration { gpa });
    }

    let initial_work = counter.read()?;
    if initial_work != 0 {
        return Err(LinuxConsoleError::NonzeroInitialWork { work: initial_work });
    }
    let clock = VClock::new(VClockConfig {
        ratio_num: 1,
        ratio_den: 1,
        guest_hz: clock_config.guest_clock_hz,
        guest_base: 0,
        vns_base: 0,
    })
    .map_err(|error| RunError::Seam {
        context: "construct the AA-5 work-derived clock",
        message: error.to_string(),
    })?;
    let arm_delta = exact_arm_delta(clock_config.refresh_delta_work, clock_config.skid_margin)?;
    counter.arm_overflow(arm_delta)?;
    let mut target = clock_config.refresh_delta_work;
    let mut arm_point = arm_delta;
    let mut publications = 0_u64;
    let mut max_refresh_gap_work = 0_u64;
    let mut last_refresh_work = initial_work;
    let mut registration_floor_work = None;
    let mut advisory_exits = 0_u64;

    let mut mmio = LinuxWorkMmio::new(console_config);
    let mut exits = 0_u64;
    while exits < console_config.max_exits {
        exits += 1;
        match vcpu.run()? {
            VcpuExit::Mmio {
                addr,
                data,
                is_write,
            } => {
                if mmio.service(vcpu, console_config, addr, &data, is_write)? {
                    registration_floor_work = Some(
                        target
                            .checked_sub(clock_config.refresh_delta_work)
                            .ok_or(LinuxConsoleError::RefreshTargetOverflow { work: target })?,
                    );
                }
                // READY is latched, never accepted at this natural MMIO exit: its live work
                // count is skid-tainted and an armed forced refresh may already be late/lost.
                // Keep running until the next exact landing publishes, then return from the
                // `Landed` arm below. The owned init spins after READY so that target remains
                // reachable instead of powering off between marker and proof.
            }
            VcpuExit::Preempt => {
                let remaining = console_config.max_exits - exits;
                match service_exact_preempt(
                    vcpu,
                    counter,
                    target,
                    arm_point,
                    clock_config.skid_margin,
                    remaining,
                    |vcpu, addr, data, is_write| {
                        if mmio.service(vcpu, console_config, addr, data, is_write)? {
                            registration_floor_work =
                                Some(target.checked_sub(clock_config.refresh_delta_work).ok_or(
                                    LinuxConsoleError::RefreshTargetOverflow { work: target },
                                )?);
                        }
                        Ok::<(), LinuxConsoleError>(())
                    },
                )? {
                    ExactPreemptOutcome::Advisory { work } => {
                        advisory_exits += 1;
                        if advisory_exits > MAX_CLOCK_ADVISORY_EXITS {
                            return Err(RunError::AdvisoryExitStorm {
                                exits: advisory_exits,
                                work,
                                target,
                            }
                            .into());
                        }
                    }
                    ExactPreemptOutcome::Landed { run_exits, .. } => {
                        exits += run_exits;
                        if let Some(registration_gpa) = vcpu.linux_pvclock_gpa() {
                            let write = if publications == 0 {
                                PvclockWrite::Canonical
                            } else {
                                PvclockWrite::Refresh
                            };
                            vcpu.publish_linux_pvclock(
                                clock.vns(target),
                                clock.guest_ticks(target),
                                clock_config.guest_clock_hz,
                                write,
                            )?;
                            publications += 1;
                            let prior_bound = if publications == 1 {
                                registration_floor_work.ok_or_else(|| RunError::Seam {
                                    context: "locate the Linux pvclock registration cadence",
                                    message: "a registered GPA has no exact-cadence floor".into(),
                                })?
                            } else {
                                last_refresh_work
                            };
                            let refresh_gap = target.checked_sub(prior_bound).ok_or_else(|| {
                                RunError::Seam {
                                    context: "advance the Linux pvclock refresh anchor",
                                    message: format!(
                                        "exact target {target} went backwards from prior anchor \
                                         {prior_bound}"
                                    ),
                                }
                            })?;
                            max_refresh_gap_work = max_refresh_gap_work.max(refresh_gap);
                            last_refresh_work = target;

                            if mmio.console.ready {
                                return Ok(LinuxWorkClockResult {
                                    boot: LinuxConsoleResult {
                                        console: mmio.console.console,
                                        exits,
                                    },
                                    publications,
                                    max_refresh_gap_work,
                                    last_refresh_work,
                                    registration_gpa,
                                });
                            }
                        }

                        let next_target = target
                            .checked_add(clock_config.refresh_delta_work)
                            .ok_or(LinuxConsoleError::RefreshTargetOverflow { work: target })?;
                        let next_arm_point = target
                            .checked_add(arm_delta)
                            .ok_or(LinuxConsoleError::RefreshTargetOverflow { work: target })?;
                        counter.arm_overflow(arm_delta)?;
                        target = next_target;
                        arm_point = next_arm_point;
                    }
                }
            }
            VcpuExit::MalformedMmio { addr, width } => {
                return Err(RunError::MalformedMmio { addr, width }.into());
            }
            VcpuExit::Other(reason) => {
                return Err(RunError::UnexpectedExit(reason).into());
            }
            VcpuExit::SignalKick => {
                return Err(LinuxConsoleError::UnexpectedMechanism("signal kick"));
            }
            VcpuExit::Debug => {
                return Err(LinuxConsoleError::UnexpectedMechanism("KVM_EXIT_DEBUG"));
            }
        }
    }
    Err(LinuxConsoleError::ExitLimit {
        limit: console_config.max_exits,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    struct Scripted {
        exits: VecDeque<VcpuExit>,
        reads: Vec<Vec<u8>>,
    }

    impl Vcpu for Scripted {
        fn run(&mut self) -> Result<VcpuExit, RunError> {
            self.exits.pop_front().ok_or(RunError::UnexpectedExit(999))
        }

        fn complete_mmio_read(&mut self, data: &[u8]) -> Result<(), RunError> {
            self.reads.push(data.to_vec());
            Ok(())
        }

        fn state_digest(&mut self) -> Result<String, RunError> {
            Ok("unused".into())
        }
    }

    struct ScriptedClockVcpu {
        exits: VecDeque<VcpuExit>,
        page: Vec<u8>,
        single_step: bool,
        registration_gpa: Option<u64>,
        mmio_reads: Vec<Vec<u8>>,
        validations: Vec<u64>,
        publications: Vec<(u64, u64, u64, u64, PvclockWrite)>,
    }

    impl Vcpu for ScriptedClockVcpu {
        fn run(&mut self) -> Result<VcpuExit, RunError> {
            self.exits.pop_front().ok_or(RunError::UnexpectedExit(999))
        }

        fn complete_mmio_read(&mut self, data: &[u8]) -> Result<(), RunError> {
            self.mmio_reads.push(data.to_vec());
            Ok(())
        }

        fn state_digest(&mut self) -> Result<String, RunError> {
            Ok("unused".into())
        }
    }

    impl StepVcpu for ScriptedClockVcpu {
        fn arm_single_step(&mut self) -> Result<(), RunError> {
            self.single_step = true;
            Ok(())
        }

        fn disarm_single_step(&mut self) -> Result<(), RunError> {
            self.single_step = false;
            Ok(())
        }

        fn pc(&mut self) -> Result<u64, RunError> {
            Ok(0)
        }

        fn opcode_at(&mut self, _addr: u64) -> Result<Option<u32>, RunError> {
            Ok(Some(0))
        }

        fn vbar(&mut self) -> Result<u64, RunError> {
            Ok(0)
        }

        fn regs_digest(&mut self) -> Result<String, RunError> {
            Ok("unused".into())
        }
    }

    impl LinuxPvclockVcpu for ScriptedClockVcpu {
        fn linux_pvclock_gpa(&self) -> Option<u64> {
            self.registration_gpa
        }

        fn register_linux_pvclock_gpa(&mut self, gpa: u64) -> Result<(), RunError> {
            self.validations.push(gpa);
            if let Some(registered) = self.registration_gpa {
                return Err(RunError::Seam {
                    context: "register scripted Linux pvclock GPA",
                    message: format!(
                        "one-shot already pinned to {registered:#x}; rejected {gpa:#x}"
                    ),
                });
            }
            if gpa != crate::linux_boot::PVCLOCK_GPA {
                return Err(RunError::Seam {
                    context: "validate scripted Linux pvclock GPA",
                    message: format!("rejected {gpa:#x}"),
                });
            }
            self.registration_gpa = Some(gpa);
            Ok(())
        }

        fn publish_linux_pvclock(
            &mut self,
            vns: u64,
            guest_clock: u64,
            guest_clock_hz: u64,
            write: PvclockWrite,
        ) -> Result<(), RunError> {
            let gpa = self.registration_gpa.ok_or_else(|| RunError::Seam {
                context: "publish scripted Linux pvclock page",
                message: "no registered GPA".into(),
            })?;
            match write {
                PvclockWrite::Canonical => {
                    vtime::pvclock::stamp_canonical(
                        &mut self.page,
                        vns,
                        guest_clock,
                        guest_clock_hz,
                    );
                }
                PvclockWrite::Refresh => {
                    vtime::pvclock::stamp(&mut self.page, vns, guest_clock, guest_clock_hz);
                }
            }
            self.publications
                .push((gpa, vns, guest_clock, guest_clock_hz, write));
            Ok(())
        }
    }

    struct ScriptedCounter {
        reads: VecDeque<u64>,
        armed: Vec<u64>,
        resumes: u64,
        rearms: u64,
    }

    impl WorkCounter for ScriptedCounter {
        fn read(&mut self) -> Result<u64, RunError> {
            self.reads.pop_front().ok_or_else(|| RunError::Seam {
                context: "scripted Linux work counter",
                message: "no scripted read remains".into(),
            })
        }

        fn arm_overflow(&mut self, delta: u64) -> Result<(), RunError> {
            self.armed.push(delta);
            Ok(())
        }

        fn rearm(&mut self) -> Result<(), RunError> {
            self.rearms += 1;
            Ok(())
        }

        fn resume_counting(&mut self) -> Result<(), RunError> {
            self.resumes += 1;
            Ok(())
        }
    }

    fn mmio(offset: u64, data: &[u8], is_write: bool) -> VcpuExit {
        VcpuExit::Mmio {
            addr: UART_BASE + offset,
            data: data.to_vec(),
            is_write,
        }
    }

    fn pvclock_register(gpa: u64) -> VcpuExit {
        VcpuExit::Mmio {
            addr: PVCLOCK_REGISTER_BASE + PVCLOCK_REGISTER_GPA_OFFSET,
            data: gpa.to_le_bytes().to_vec(),
            is_write: true,
        }
    }

    fn scripted_clock_vcpu(exits: VecDeque<VcpuExit>, fill: u8) -> ScriptedClockVcpu {
        ScriptedClockVcpu {
            exits,
            page: vec![fill; vtime::pvclock::PVCLOCK_PAGE_LEN],
            single_step: false,
            registration_gpa: None,
            mmio_reads: Vec::new(),
            validations: Vec::new(),
            publications: Vec::new(),
        }
    }

    fn config(marker: &[u8]) -> LinuxConsoleConfig {
        LinuxConsoleConfig {
            ready_marker: marker.to_vec(),
            max_exits: 100,
            max_console_bytes: 100,
        }
    }

    #[test]
    fn services_primecell_reads_and_stops_on_the_userspace_marker() {
        let mut exits = VecDeque::from([
            mmio(0x30, &[1, 0, 0, 0], true),
            mmio(0xfe0, &[0; 4], false),
            mmio(PL011_FR_OFFSET, &[0; 4], false),
        ]);
        for byte in b"boot\nREADY" {
            exits.push_back(mmio(PL011_DR_OFFSET, &[*byte], true));
        }
        // Must not be consumed after the first complete marker.
        exits.push_back(VcpuExit::Other(42));
        let mut vcpu = Scripted {
            exits,
            reads: Vec::new(),
        };

        let result = run_until_ready(&mut vcpu, &config(b"READY")).unwrap();
        assert_eq!(result.console, b"boot\nREADY");
        assert_eq!(result.exits, 13);
        assert_eq!(vcpu.reads[0], 0x11u64.to_le_bytes()[..4]);
        assert_eq!(vcpu.reads[1], PL011_FR_TXFE_RXFE.to_le_bytes()[..4]);
        assert_eq!(vcpu.exits.len(), 1);
    }

    #[test]
    fn refuses_vacuous_or_unbounded_configurations() {
        let mut vcpu = Scripted {
            exits: VecDeque::new(),
            reads: Vec::new(),
        };
        assert!(matches!(
            run_until_ready(&mut vcpu, &config(b"")),
            Err(LinuxConsoleError::EmptyReadyMarker)
        ));
        let mut zero = config(b"x");
        zero.max_exits = 0;
        assert!(matches!(
            run_until_ready(&mut vcpu, &zero),
            Err(LinuxConsoleError::ZeroExitBudget)
        ));
        let mut short = config(b"long");
        short.max_console_bytes = 3;
        assert!(matches!(
            run_until_ready(&mut vcpu, &short),
            Err(LinuxConsoleError::MarkerExceedsConsoleLimit { .. })
        ));
        let mut huge_marker = config(&vec![b'x'; MAX_MARKER_BYTES + 1]);
        huge_marker.max_console_bytes = MAX_MARKER_BYTES + 1;
        assert!(matches!(
            run_until_ready(&mut vcpu, &huge_marker),
            Err(LinuxConsoleError::MarkerTooLarge { .. })
        ));
        let mut huge_console = config(b"x");
        huge_console.max_console_bytes = MAX_CONSOLE_BYTES + 1;
        assert!(matches!(
            run_until_ready(&mut vcpu, &huge_console),
            Err(LinuxConsoleError::ConsoleBoundTooLarge { .. })
        ));
        let mut huge_exits = config(b"x");
        huge_exits.max_exits = MAX_KVM_EXITS + 1;
        assert!(matches!(
            run_until_ready(&mut vcpu, &huge_exits),
            Err(LinuxConsoleError::ExitBoundTooLarge { .. })
        ));
    }

    #[test]
    fn bounds_exits_and_console_and_refuses_non_uart_mmio() {
        let mut exit_bound = Scripted {
            exits: VecDeque::from([mmio(0x30, &[0; 4], true)]),
            reads: Vec::new(),
        };
        let mut one = config(b"x");
        one.max_exits = 1;
        assert!(matches!(
            run_until_ready(&mut exit_bound, &one),
            Err(LinuxConsoleError::ExitLimit { limit: 1 })
        ));

        let mut console_bound = Scripted {
            exits: VecDeque::from([
                mmio(0, b"a", true),
                mmio(0, b"b", true),
                mmio(0, b"c", true),
            ]),
            reads: Vec::new(),
        };
        let mut two = config(b"z");
        two.max_console_bytes = 2;
        assert!(matches!(
            run_until_ready(&mut console_bound, &two),
            Err(LinuxConsoleError::ConsoleLimit { limit: 2 })
        ));

        let mut bad_mmio = Scripted {
            exits: VecDeque::from([VcpuExit::Mmio {
                addr: UART_BASE - 1,
                data: vec![0],
                is_write: true,
            }]),
            reads: Vec::new(),
        };
        assert!(matches!(
            run_until_ready(&mut bad_mmio, &config(b"x")),
            Err(LinuxConsoleError::Run(RunError::UnexpectedMmio { .. }))
        ));

        let mut wide_read = Scripted {
            exits: VecDeque::from([mmio(PL011_FR_OFFSET, &[0; 9], false)]),
            reads: Vec::new(),
        };
        assert!(matches!(
            run_until_ready(&mut wide_read, &config(b"x")),
            Err(LinuxConsoleError::UnsupportedMmioWidth { width: 9, .. })
        ));

        for width in [3, 8] {
            let mut unsupported = Scripted {
                exits: VecDeque::from([mmio(PL011_FR_OFFSET, &vec![0; width], false)]),
                reads: Vec::new(),
            };
            assert!(matches!(
                run_until_ready(&mut unsupported, &config(b"x")),
                Err(LinuxConsoleError::UnsupportedMmioWidth {
                    width: found,
                    ..
                }) if found == width
            ));
        }

        let mut straddling = Scripted {
            exits: VecDeque::from([mmio(PL011_PAGE - 1, &[0; 4], false)]),
            reads: Vec::new(),
        };
        assert!(matches!(
            run_until_ready(&mut straddling, &config(b"x")),
            Err(LinuxConsoleError::Run(RunError::UnexpectedMmio { .. }))
        ));

        let mut malformed_seam = Scripted {
            exits: VecDeque::from([VcpuExit::MalformedMmio {
                addr: UART_BASE,
                width: 9,
            }]),
            reads: Vec::new(),
        };
        assert!(matches!(
            run_until_ready(&mut malformed_seam, &config(b"x")),
            Err(LinuxConsoleError::Run(RunError::MalformedMmio {
                width: 9,
                ..
            }))
        ));
    }

    #[test]
    fn marker_matcher_is_linear_and_handles_overlapping_prefixes() {
        let mut matcher = MarkerMatcher::new(b"aaab");
        let matches: Vec<bool> = b"aaaaab".iter().map(|byte| matcher.push(*byte)).collect();
        assert_eq!(matches, [false, false, false, false, false, true]);
    }

    #[test]
    fn pvclock_registration_is_validated_one_shot_and_reports_abi() {
        let config = config(b"R");
        let mut mmio = LinuxWorkMmio::new(&config);
        let mut vcpu = scripted_clock_vcpu(VecDeque::new(), 0);

        mmio.service(
            &mut vcpu,
            &config,
            PVCLOCK_REGISTER_BASE + PVCLOCK_REGISTER_ABI_OFFSET,
            &[0; 4],
            false,
        )
        .unwrap();
        assert_eq!(vcpu.mmio_reads, [0_u32.to_le_bytes()]);

        for (addr, data, is_write) in [
            (PVCLOCK_REGISTER_BASE, vec![0; 4], true),
            (
                PVCLOCK_REGISTER_BASE + PVCLOCK_REGISTER_ABI_OFFSET,
                vec![0; 8],
                false,
            ),
            (
                PVCLOCK_REGISTER_BASE + PVCLOCK_REGISTER_ABI_OFFSET,
                vec![0; 4],
                true,
            ),
            (PVCLOCK_REGISTER_END - 1, vec![0; 4], false),
        ] {
            assert!(matches!(
                mmio.service(&mut vcpu, &config, addr, &data, is_write),
                Err(LinuxConsoleError::InvalidPvclockRegisterMmio { .. })
            ));
        }
        assert_eq!(vcpu.registration_gpa, None);

        let rejected = (PVCLOCK_GPA + 1).to_le_bytes();
        assert!(matches!(
            mmio.service(
                &mut vcpu,
                &config,
                PVCLOCK_REGISTER_BASE + PVCLOCK_REGISTER_GPA_OFFSET,
                &rejected,
                true,
            ),
            Err(LinuxConsoleError::Run(RunError::Seam { .. }))
        ));
        assert_eq!(
            vcpu.registration_gpa, None,
            "a rejection cannot consume the one-shot"
        );

        mmio.service(
            &mut vcpu,
            &config,
            PVCLOCK_REGISTER_BASE + PVCLOCK_REGISTER_GPA_OFFSET,
            &PVCLOCK_GPA.to_le_bytes(),
            true,
        )
        .unwrap();
        mmio.service(
            &mut vcpu,
            &config,
            PVCLOCK_REGISTER_BASE + PVCLOCK_REGISTER_ABI_OFFSET,
            &[0; 4],
            false,
        )
        .unwrap();
        assert_eq!(
            vcpu.mmio_reads,
            [
                0_u32.to_le_bytes().to_vec(),
                vtime::pvclock::PVCLOCK_ABI_VERSION.to_le_bytes().to_vec(),
            ]
        );

        let mut reentered_loop = LinuxWorkMmio::new(&config);
        assert!(matches!(
            reentered_loop.service(
                &mut vcpu,
                &config,
                PVCLOCK_REGISTER_BASE + PVCLOCK_REGISTER_GPA_OFFSET,
                &PVCLOCK_GPA.to_le_bytes(),
                true,
            ),
            Err(LinuxConsoleError::PvclockReregister {
                registered: PVCLOCK_GPA,
                attempted: PVCLOCK_GPA,
            })
        ));
        assert_eq!(vcpu.validations, [PVCLOCK_GPA + 1, PVCLOCK_GPA]);
    }

    #[test]
    fn pvclock_gpa_validation_is_total_at_every_ram_boundary() {
        const BASE: u64 = 0x4000_0000;
        const LEN: usize = 0x3000;

        assert_eq!(
            linux_pvclock_page_range(BASE, BASE, LEN).unwrap(),
            0..0x1000
        );
        assert_eq!(
            linux_pvclock_page_range(BASE + 0x2000, BASE, LEN).unwrap(),
            0x2000..0x3000
        );
        for gpa in [BASE - 0x1000, BASE + 1, BASE + 0x3000, u64::MAX] {
            assert!(linux_pvclock_page_range(gpa, BASE, LEN).is_err());
        }
        assert!(
            linux_pvclock_page_range(!0xfff_u64, 0, usize::MAX).is_err(),
            "the page-end addition must fail rather than wrapping"
        );
    }

    #[test]
    fn work_clock_rejects_vacuous_or_guest_unreachable_cadences() {
        let console = config(b"R");
        let mut vcpu = scripted_clock_vcpu(VecDeque::new(), 0);

        for (clock, expected) in [
            (
                LinuxWorkClockConfig {
                    refresh_delta_work: 0,
                    skid_margin: 1,
                    guest_clock_hz: 50_000_000,
                },
                "zero",
            ),
            (
                LinuxWorkClockConfig {
                    refresh_delta_work: MAX_REFRESH_DELTA_WORK + 1,
                    skid_margin: 1,
                    guest_clock_hz: 50_000_000,
                },
                "large",
            ),
            (
                LinuxWorkClockConfig {
                    refresh_delta_work: 23,
                    skid_margin: 1,
                    guest_clock_hz: 0,
                },
                "frequency",
            ),
        ] {
            let mut counter = ScriptedCounter {
                reads: VecDeque::new(),
                armed: Vec::new(),
                resumes: 0,
                rearms: 0,
            };
            let error =
                run_until_ready_work_clock(&mut vcpu, &mut counter, &console, clock).unwrap_err();
            assert!(
                matches!(
                    (&error, expected),
                    (LinuxConsoleError::ZeroRefreshDelta, "zero")
                        | (LinuxConsoleError::RefreshDeltaTooLarge { .. }, "large")
                        | (LinuxConsoleError::ZeroGuestClockHz, "frequency")
                ),
                "unexpected {expected} validation error: {error}"
            );
        }

        let mut counter = ScriptedCounter {
            reads: VecDeque::from([1]),
            armed: Vec::new(),
            resumes: 0,
            rearms: 0,
        };
        assert!(matches!(
            run_until_ready_work_clock(
                &mut vcpu,
                &mut counter,
                &console,
                LinuxWorkClockConfig {
                    refresh_delta_work: 23,
                    skid_margin: 1,
                    guest_clock_hz: 50_000_000,
                },
            ),
            Err(LinuxConsoleError::NonzeroInitialWork { work: 1 })
        ));

        let mut pre_registered = scripted_clock_vcpu(VecDeque::new(), 0);
        pre_registered.registration_gpa = Some(PVCLOCK_GPA);
        let mut untouched_counter = ScriptedCounter {
            reads: VecDeque::new(),
            armed: Vec::new(),
            resumes: 0,
            rearms: 0,
        };
        assert!(matches!(
            run_until_ready_work_clock(
                &mut pre_registered,
                &mut untouched_counter,
                &console,
                LinuxWorkClockConfig {
                    refresh_delta_work: 23,
                    skid_margin: 1,
                    guest_clock_hz: 50_000_000,
                },
            ),
            Err(LinuxConsoleError::PreexistingPvclockRegistration { gpa: PVCLOCK_GPA })
        ));
        assert!(untouched_counter.armed.is_empty());
    }

    #[test]
    fn registration_during_the_exact_walk_publishes_only_at_the_landing() {
        let mut exits = VecDeque::from([
            mmio(PL011_DR_OFFSET, b"R", true),
            VcpuExit::Preempt,
            pvclock_register(PVCLOCK_GPA),
        ]);
        for _ in 0..5 {
            exits.push_back(VcpuExit::Debug);
        }
        let mut vcpu = scripted_clock_vcpu(exits, 0xa5);
        let mut counter = ScriptedCounter {
            reads: VecDeque::from([0, 18, 19, 20, 21, 22, 23]),
            armed: Vec::new(),
            resumes: 0,
            rearms: 0,
        };

        let result = run_until_ready_work_clock(
            &mut vcpu,
            &mut counter,
            &config(b"R"),
            LinuxWorkClockConfig {
                refresh_delta_work: 23,
                skid_margin: 1,
                guest_clock_hz: 50_000_000,
            },
        )
        .unwrap();

        assert_eq!(result.boot.exits, 8);
        assert_eq!(result.registration_gpa, PVCLOCK_GPA);
        assert_eq!(result.last_refresh_work, 23);
        assert_eq!(vcpu.publications.len(), 1);
        assert_eq!(vcpu.publications[0].4, PvclockWrite::Canonical);
    }

    #[test]
    fn work_clock_publishes_only_after_exact_landing_and_finishes_the_step() {
        // First Preempt is an unrelated host IRQ below the arm point; the page must not move.
        // The second is the real arm-early overflow.
        let mut exits = VecDeque::from([
            pvclock_register(PVCLOCK_GPA),
            VcpuExit::Preempt,
            VcpuExit::Preempt,
        ]);
        // Each stepped PL011 write exits once for MMIO and once for the debug landing. The
        // marker completes before the exact work target, but the executor must finish landing,
        // publish the page, and disarm debug before it returns.
        for byte in b"READY" {
            exits.push_back(mmio(PL011_DR_OFFSET, &[*byte], true));
            exits.push_back(VcpuExit::Debug);
        }
        let mut vcpu = scripted_clock_vcpu(exits, 0xa5);
        let mut counter = ScriptedCounter {
            // Pre-entry zero; advisory at 3; overflow at 18; five steps reach target 23.
            reads: VecDeque::from([0, 3, 18, 19, 20, 21, 22, 23]),
            armed: Vec::new(),
            resumes: 0,
            rearms: 0,
        };

        let result = run_until_ready_work_clock(
            &mut vcpu,
            &mut counter,
            &config(b"READY"),
            LinuxWorkClockConfig {
                refresh_delta_work: 23,
                skid_margin: 1,
                guest_clock_hz: 50_000_000,
            },
        )
        .unwrap();

        assert_eq!(result.boot.console, b"READY");
        assert_eq!(result.boot.exits, 13);
        assert_eq!(result.publications, 1);
        assert_eq!(result.max_refresh_gap_work, 23);
        assert_eq!(result.last_refresh_work, 23);
        assert_eq!(result.registration_gpa, PVCLOCK_GPA);
        assert!(!vcpu.single_step, "debug must be disarmed before return");
        assert_eq!(counter.armed, [6]); // 23 - skid(1) - canonical-PC headroom(16)
        assert_eq!(counter.resumes, 1);
        assert_eq!(counter.rearms, 1);
        assert_eq!(
            vcpu.publications,
            [(PVCLOCK_GPA, 23, 1, 50_000_000, PvclockWrite::Canonical)]
        );
        let page = vtime::pvclock::read(&vcpu.page).unwrap();
        assert_eq!(page.seq, 0);
        assert_eq!(page.vns, 23);
        assert_eq!(page.guest_clock, 1);
        assert_eq!(page.flags, vtime::pvclock::PVCLOCK_FLAGS_V1);
        assert!(
            vcpu.page[vtime::pvclock::RESERVED_OFF..]
                .iter()
                .all(|byte| *byte == 0)
        );
    }

    #[test]
    fn work_clock_latches_an_early_marker_but_waits_for_exact_refresh() {
        let mut exits = VecDeque::from([
            pvclock_register(PVCLOCK_GPA),
            mmio(PL011_DR_OFFSET, b"R", true),
            VcpuExit::Preempt,
        ]);
        for _ in 0..5 {
            exits.push_back(VcpuExit::Debug);
        }
        let mut vcpu = scripted_clock_vcpu(exits, 0);
        let mut counter = ScriptedCounter {
            reads: VecDeque::from([0, 18, 19, 20, 21, 22, 23]),
            armed: Vec::new(),
            resumes: 0,
            rearms: 0,
        };
        let result = run_until_ready_work_clock(
            &mut vcpu,
            &mut counter,
            &config(b"R"),
            LinuxWorkClockConfig {
                refresh_delta_work: 23,
                skid_margin: 1,
                guest_clock_hz: 50_000_000,
            },
        )
        .unwrap();
        assert_eq!(result.boot.console, b"R");
        assert_eq!(result.boot.exits, 8);
        assert_eq!(result.publications, 1);
        assert_eq!(result.last_refresh_work, 23);
        assert_eq!(result.registration_gpa, PVCLOCK_GPA);
        assert_eq!(counter.armed, [6]);
        assert_eq!(vcpu.publications.len(), 1);
    }

    #[test]
    fn work_clock_rearms_and_publishes_a_second_exact_period() {
        let mut exits = VecDeque::from([pvclock_register(PVCLOCK_GPA), VcpuExit::Preempt]);
        for _ in 0..5 {
            exits.push_back(VcpuExit::Debug);
        }
        // READY arrives naturally after the first publication. It remains latched until the
        // second forced target proves that periodic rearming, exact landing, and refresh all
        // completed before success is reported.
        exits.push_back(mmio(PL011_DR_OFFSET, b"R", true));
        exits.push_back(VcpuExit::Preempt);
        for _ in 0..5 {
            exits.push_back(VcpuExit::Debug);
        }
        let mut vcpu = scripted_clock_vcpu(exits, 0);
        let mut counter = ScriptedCounter {
            reads: VecDeque::from([0, 18, 19, 20, 21, 22, 23, 41, 42, 43, 44, 45, 46]),
            armed: Vec::new(),
            resumes: 0,
            rearms: 0,
        };

        let result = run_until_ready_work_clock(
            &mut vcpu,
            &mut counter,
            &config(b"R"),
            LinuxWorkClockConfig {
                refresh_delta_work: 23,
                skid_margin: 1,
                guest_clock_hz: 50_000_000,
            },
        )
        .unwrap();

        assert_eq!(result.boot.console, b"R");
        assert_eq!(result.boot.exits, 14);
        assert_eq!(result.publications, 2);
        assert_eq!(result.max_refresh_gap_work, 23);
        assert_eq!(result.last_refresh_work, 46);
        assert_eq!(result.registration_gpa, PVCLOCK_GPA);
        assert!(!vcpu.single_step);
        assert_eq!(counter.armed, [6, 6]);
        assert_eq!(counter.resumes, 2);
        assert_eq!(counter.rearms, 0);
        assert_eq!(
            vcpu.publications,
            [
                (PVCLOCK_GPA, 23, 1, 50_000_000, PvclockWrite::Canonical),
                (PVCLOCK_GPA, 46, 2, 50_000_000, PvclockWrite::Refresh),
            ]
        );
        let page = vtime::pvclock::read(&vcpu.page).unwrap();
        assert_eq!(page.seq, 2);
        assert_eq!(page.vns, 46);
        assert_eq!(page.guest_clock, 2);
    }

    #[test]
    fn late_registration_uses_the_prior_exact_cadence_as_its_gap_floor() {
        let mut exits = VecDeque::from([VcpuExit::Preempt]);
        for _ in 0..5 {
            exits.push_back(VcpuExit::Debug);
        }
        // The first exact target has no registered page and therefore cannot publish. The
        // registration occurs in the following cadence interval; its conservative G3 baseline
        // is the prior exact target (23), not pre-entry work zero.
        exits.push_back(pvclock_register(PVCLOCK_GPA));
        exits.push_back(mmio(PL011_DR_OFFSET, b"R", true));
        exits.push_back(VcpuExit::Preempt);
        for _ in 0..5 {
            exits.push_back(VcpuExit::Debug);
        }
        let mut vcpu = scripted_clock_vcpu(exits, 0);
        let mut counter = ScriptedCounter {
            reads: VecDeque::from([0, 18, 19, 20, 21, 22, 23, 41, 42, 43, 44, 45, 46]),
            armed: Vec::new(),
            resumes: 0,
            rearms: 0,
        };

        let result = run_until_ready_work_clock(
            &mut vcpu,
            &mut counter,
            &config(b"R"),
            LinuxWorkClockConfig {
                refresh_delta_work: 23,
                skid_margin: 1,
                guest_clock_hz: 50_000_000,
            },
        )
        .unwrap();

        assert_eq!(result.boot.exits, 14);
        assert_eq!(result.publications, 1);
        assert_eq!(result.max_refresh_gap_work, 23);
        assert_eq!(result.last_refresh_work, 46);
        assert_eq!(result.registration_gpa, PVCLOCK_GPA);
        assert_eq!(counter.armed, [6, 6]);
        assert_eq!(
            vcpu.publications,
            [(PVCLOCK_GPA, 46, 2, 50_000_000, PvclockWrite::Canonical)]
        );
    }
}
