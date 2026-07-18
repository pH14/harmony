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

use crate::run::{RunError, Vcpu, VcpuExit};

const PL011_PAGE: u64 = 0x1000;
const PL011_DR_OFFSET: u64 = 0x000;
const PL011_FR_OFFSET: u64 = 0x018;
const PL011_FR_TXFE_RXFE: u64 = (1 << 7) | (1 << 4);
/// Marker the owned AA-5 initramfs prints after `/init` reaches userspace.
pub const LINUX_READY_MARKER: &[u8] = b"HARMONY_AA5_READY";
/// Hard operational ceiling above the ordinary command default.
pub const MAX_CONSOLE_BYTES: usize = 64 << 20;
/// Hard operational exit ceiling above the ordinary command default.
pub const MAX_KVM_EXITS: u64 = 100_000_000;
const MAX_MARKER_BYTES: usize = 4096;

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

/// Streaming Knuth-Morris-Pratt matcher: one amortized-constant update per
/// console byte, so a long repeated near-match cannot turn bounded input into
/// quadratic work.
struct MarkerMatcher {
    needle: Vec<u8>,
    prefix: Vec<usize>,
    matched: usize,
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

    let mut console = Vec::new();
    let mut marker = MarkerMatcher::new(&config.ready_marker);
    for exits in 1..=config.max_exits {
        match vcpu.run()? {
            VcpuExit::Mmio {
                addr,
                data,
                is_write,
            } => {
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
                    if offset != PL011_DR_OFFSET {
                        continue;
                    }
                    if console.len() == config.max_console_bytes {
                        return Err(LinuxConsoleError::ConsoleLimit {
                            limit: config.max_console_bytes,
                        });
                    }
                    console.push(data[0]);
                    if marker.push(data[0]) {
                        return Ok(LinuxConsoleResult { console, exits });
                    }
                } else {
                    let bytes = read_value(offset).to_le_bytes();
                    let response = &bytes[..data.len()];
                    vcpu.complete_mmio_read(response)?;
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

    fn mmio(offset: u64, data: &[u8], is_write: bool) -> VcpuExit {
        VcpuExit::Mmio {
            addr: UART_BASE + offset,
            data: data.to_vec(),
            is_write,
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
}
