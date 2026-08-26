// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stage 0 — the Linux half: reading the chip and the host.
//!
//! Everything here is a `/proc` read, a `/sys` read, an MSR read, or a
//! `perf_event_open`. The portable half in [`crate::stage0`] turns what this
//! produces into expect-versus-found rows.
//!
//! A read that fails is a refusal, never a missing row. A condition the chip's
//! entry requires and this module cannot read stops the stage.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[cfg(target_arch = "x86_64")]
use crate::chips::PmuShape;
use crate::chips::{ChipEntry, ChipIdentity, HostConditionKind, TableValue, Vendor, match_chip};
use crate::pack::Pack;
use crate::payload;
use crate::perf::Scope;
use crate::perf_sys::{PerfCounter, allowed_core_count, current_core};
use crate::stage0::{
    Reading, Row, Stage0Error, Stage0Outcome, WorkClockProbe, build_rows, normalize_bool,
    parse_cpu_list,
};
#[cfg(target_arch = "x86_64")]
use crate::stage0::{cpuinfo_field, cpuinfo_first_stanza, normalize_revision};

/// The MSR holding AMD's load-store configuration, including the speculative
/// lock-mapping bit rr's Zen workaround sets.
const LS_CFG: u64 = 0xC001_1020;

/// The speculative lock-mapping bit in [`LS_CFG`]. Set means the speculative
/// mapping is disabled.
const LS_CFG_SPEC_LOCK_MAP_BIT: u32 = 54;

/// Iterations the work-clock probe runs. Large enough that a counter that is
/// merely noisy still reads far from zero, small enough to finish instantly.
const WORK_CLOCK_PROBE_ITERATIONS: u64 = 1_000_000;

fn read_file(what: &str, path: impl AsRef<Path>) -> Result<String, Stage0Error> {
    let path = path.as_ref();
    fs::read_to_string(path).map_err(|e| Stage0Error::Read {
        what: format!("{what} ({})", path.display()),
        detail: e.to_string(),
    })
}

fn read_optional(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Read the chip's identity: vendor, family, model, stepping, and the microcode
/// or firmware revision the kernel records.
///
/// # Errors
/// [`Stage0Error::Read`] when a source the identity needs cannot be read or
/// cannot be parsed.
#[cfg(target_arch = "x86_64")]
pub fn read_chip_identity() -> Result<ChipIdentity, Stage0Error> {
    let text = read_file("chip identity", "/proc/cpuinfo")?;
    let fields = cpuinfo_first_stanza(&text);
    let unparsed = |what: &str| Stage0Error::Read {
        what: format!("{what} in /proc/cpuinfo"),
        detail: "field is absent or not a number".to_string(),
    };

    let vendor_id = cpuinfo_field(&fields, "vendor_id").ok_or_else(|| unparsed("vendor_id"))?;
    let vendor = match vendor_id {
        "GenuineIntel" => Vendor::GenuineIntel,
        "AuthenticAMD" => Vendor::AuthenticAMD,
        other => {
            return Err(Stage0Error::Read {
                what: "vendor_id in /proc/cpuinfo".to_string(),
                detail: format!("{other:?} is neither GenuineIntel nor AuthenticAMD"),
            });
        }
    };
    let number = |key: &str| -> Result<u32, Stage0Error> {
        cpuinfo_field(&fields, key)
            .and_then(|v| v.parse::<u32>().ok())
            .ok_or_else(|| unparsed(key))
    };

    // The kernel prints family and model already folded with their extended
    // fields, which is the spelling the known-chip table's match rules use.
    Ok(ChipIdentity {
        vendor,
        family: number("cpu family")?,
        model: number("model")?,
        stepping: number("stepping")?,
        midr: 0,
        microcode_rev: read_optional("/sys/devices/system/cpu/cpu0/microcode/version")
            .or_else(|| cpuinfo_field(&fields, "microcode").map(str::to_string))
            .and_then(|raw| normalize_revision(&raw)),
    })
}

/// Read the chip's identity from `MIDR_EL1`.
///
/// # Errors
/// [`Stage0Error::Read`] when `MIDR_EL1` cannot be read or parsed.
#[cfg(target_arch = "aarch64")]
pub fn read_chip_identity() -> Result<ChipIdentity, Stage0Error> {
    const MIDR: &str = "/sys/devices/system/cpu/cpu0/regs/identification/midr_el1";
    let raw = read_file("MIDR_EL1", MIDR)?;
    let trimmed = raw.trim();
    let digits = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    let midr = u64::from_str_radix(digits, 16).map_err(|e| Stage0Error::Read {
        what: format!("MIDR_EL1 ({MIDR})"),
        detail: format!("{trimmed:?} is not hexadecimal: {e}"),
    })?;
    Ok(ChipIdentity {
        vendor: Vendor::Aarch64,
        family: 0,
        model: 0,
        stepping: 0,
        midr,
        // No firmware-revision source on aarch64 corresponds to the x86
        // microcode revision, so nothing is claimed for it.
        microcode_rev: None,
    })
}

/// The online CPUs, from `/sys/devices/system/cpu/online`.
///
/// # Errors
/// [`Stage0Error::Read`] when the list cannot be read or parses to nothing.
pub fn online_cpus() -> Result<Vec<usize>, Stage0Error> {
    const ONLINE: &str = "/sys/devices/system/cpu/online";
    let text = read_file("online CPU list", ONLINE)?;
    let cpus = parse_cpu_list(&text);
    if cpus.is_empty() {
        return Err(Stage0Error::Read {
            what: format!("online CPU list ({ONLINE})"),
            detail: format!("{:?} names no CPU", text.trim()),
        });
    }
    Ok(cpus)
}

/// Read one MSR on one CPU through `/dev/cpu/N/msr`.
fn read_msr(cpu: usize, msr: u64) -> Result<u64, Stage0Error> {
    let path = format!("/dev/cpu/{cpu}/msr");
    let open_failed = |detail: String| Stage0Error::Read {
        what: format!("MSR {msr:#x} on cpu{cpu} ({path})"),
        detail,
    };
    let mut file = fs::File::open(&path).map_err(|e| open_failed(e.to_string()))?;
    file.seek(SeekFrom::Start(msr))
        .map_err(|e| open_failed(e.to_string()))?;
    let mut bytes = [0u8; 8];
    file.read_exact(&mut bytes)
        .map_err(|e| open_failed(e.to_string()))?;
    Ok(u64::from_le_bytes(bytes))
}

/// The KVM modules whose identity stage 0 records, in the order they are looked
/// for. The vendor module comes first because it is the one the machinery
/// patches.
fn kvm_modules(vendor: Vendor) -> &'static [&'static str] {
    match vendor {
        Vendor::GenuineIntel => &["kvm_intel", "kvm"],
        Vendor::AuthenticAMD => &["kvm_amd", "kvm"],
        Vendor::Aarch64 => &["kvm"],
    }
}

/// The CPUs the kernel command line keeps its own work off.
///
/// `isolcpus=` takes an optional list of flag words before the CPU list, and
/// the list itself mixes single CPUs with ranges, so anything that is not a
/// number or a range is skipped rather than treated as an error.
fn isolated_cpus(cmdline: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for param in cmdline.split_whitespace() {
        let Some(list) = param.strip_prefix("isolcpus=") else {
            continue;
        };
        for item in list.split(',') {
            if let Some((lo, hi)) = item.split_once('-') {
                if let (Ok(lo), Ok(hi)) = (lo.parse::<usize>(), hi.parse::<usize>()) {
                    out.extend(lo..=hi);
                }
            } else if let Ok(cpu) = item.parse::<usize>() {
                out.push(cpu);
            }
        }
    }
    out
}

/// Read every condition the chip's entry requires.
///
/// # Errors
/// [`Stage0Error::Read`] when a required source cannot be read.
pub fn read_conditions(entry: &ChipEntry, cpus: &[usize]) -> Result<Vec<Reading>, Stage0Error> {
    let mut readings = Vec::new();
    for kind in entry.host_conditions {
        match kind {
            HostConditionKind::PerfSampleCeiling => {
                // Two knobs suppress sampling interrupts, and a campaign that
                // arms at ten thousand work units trips both at their stock
                // settings. The arms whose interrupt the kernel suppresses
                // cannot be accounted for, so the pack states what each must
                // read and stage 0 confirms it before stage 1 measures.
                let rate = read_file(
                    "the sampling rate ceiling",
                    "/proc/sys/kernel/perf_event_max_sample_rate",
                )?;
                readings.push(Reading::new(*kind, "max-sample-rate", rate.trim()));
                let percent = read_file(
                    "the dynamic sampling throttle",
                    "/proc/sys/kernel/perf_cpu_time_max_percent",
                )?;
                readings.push(Reading::new(*kind, "cpu-time-max-percent", percent.trim()));
            }
            HostConditionKind::CoreIsolated => {
                // An overflow interrupt that the kernel delays past the guest's
                // deadline is what turns a landing into an overshoot, and the
                // kernel's own work on the core is the thing that delays it.
                // Pinning the measurement thread says nothing about whether the
                // kernel keeps off that core, so the isolation is read from the
                // command line and checked against the core actually in use.
                let cmdline = read_file("the kernel command line", "/proc/cmdline")?;
                let isolated = usize::try_from(current_core())
                    .is_ok_and(|core| isolated_cpus(&cmdline).contains(&core));
                let found = if isolated { "isolated" } else { "not isolated" };
                readings.push(Reading::new(*kind, "host", found));
            }
            HostConditionKind::NmiWatchdogOff => {
                let raw = read_file("NMI watchdog", "/proc/sys/kernel/nmi_watchdog")?;
                let found = if raw.trim() == "0" { "off" } else { "on" };
                readings.push(Reading::new(*kind, "host", found));
            }
            HostConditionKind::GovernorPinned => {
                for cpu in cpus {
                    let path = format!("/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_governor");
                    let found = read_optional(&path).unwrap_or_else(|| "unreadable".to_string());
                    readings.push(Reading::new(*kind, format!("cpu{cpu}"), found));
                }
            }
            HostConditionKind::SmtPolicy => {
                let found = read_optional("/sys/devices/system/cpu/smt/control")
                    .unwrap_or_else(|| "unreadable".to_string());
                readings.push(Reading::new(*kind, "host", found));
            }
            HostConditionKind::KvmPresent => {
                readings.push(Reading::new(*kind, "host", dev_kvm_state()));
            }
            HostConditionKind::KvmModuleIdentity => {
                for module in kvm_modules(entry.vendor) {
                    readings.push(Reading::new(*kind, *module, kvm_module_identity(module)));
                }
            }
            HostConditionKind::CorePinning => {
                let allowed = allowed_core_count().map_err(|e| Stage0Error::Read {
                    what: "the thread's CPU affinity".to_string(),
                    detail: e.to_string(),
                })?;
                let found = if allowed == 1 {
                    format!("pinned to cpu{}", current_core())
                } else {
                    format!("{allowed} cores allowed")
                };
                readings.push(Reading::new(*kind, "host", found));
            }
            HostConditionKind::SpecLockMapDisabled => {
                for cpu in cpus {
                    // A register that cannot be read is a reading, not a stop. KVM
                    // refuses this one to a guest, and a run that dies here reports
                    // nothing at all about the conditions it could have checked.
                    // The reading still fails the comparison against the pack.
                    let found = match read_msr(*cpu, LS_CFG) {
                        Ok(value) => {
                            let set = (value >> LS_CFG_SPEC_LOCK_MAP_BIT) & 1 == 1;
                            if set { "disabled" } else { "enabled" }.to_string()
                        }
                        Err(e) => format!("unreadable: {e}"),
                    };
                    readings.push(Reading::new(*kind, format!("cpu{cpu}"), found));
                }
            }
            HostConditionKind::SsbMitigationPinned => {
                // The kernel's speculative-store-bypass mitigation writes the same
                // register as the speculative lock-mapping workaround, so the
                // mode has to be fixed on the command line rather than left for
                // the kernel to choose per task.
                let cmdline = read_file("kernel command line", "/proc/cmdline")?;
                let found = cmdline
                    .split_whitespace()
                    .find_map(|word| word.strip_prefix("spec_store_bypass_disable="))
                    .map_or_else(|| "unset".to_string(), str::to_string);
                readings.push(Reading::new(*kind, "host", found));
            }
            HostConditionKind::AvicOff => {
                let found = read_optional("/sys/module/kvm_amd/parameters/avic")
                    .map_or_else(|| "unreadable".to_string(), |v| normalize_bool(&v));
                readings.push(Reading::new(*kind, "host", found));
            }
        }
    }
    Ok(readings)
}

/// Whether `/dev/kvm` exists and opens read-write.
fn dev_kvm_state() -> String {
    if !Path::new("/dev/kvm").exists() {
        return "absent".to_string();
    }
    match fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")
    {
        Ok(_) => "present, read-write".to_string(),
        Err(e) => format!("present, not usable: {e}"),
    }
}

/// A loaded KVM module's identity, by content: the checksum the kernel records
/// over the module's source and the build identity of the object it was
/// compiled into. A patched module differs from a stock one in both.
fn kvm_module_identity(module: &str) -> String {
    let sys = format!("/sys/module/{module}");
    if !Path::new(&sys).exists() {
        return "not loaded".to_string();
    }
    let srcversion =
        read_optional(format!("{sys}/srcversion")).unwrap_or_else(|| "unrecorded".to_string());
    let build_id = fs::read(format!("{sys}/notes/.note.gnu.build-id"))
        .ok()
        .filter(|bytes| bytes.len() > 16)
        .map_or_else(
            || "unrecorded".to_string(),
            |bytes| {
                // An ELF note is a 12-byte header, then the four-byte name
                // "GNU\0", then the identifier itself.
                bytes[16..]
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            },
        );
    format!("srcversion {srcversion}, build-id {build_id}")
}

/// Open the work-clock event and see how it behaves.
///
/// # Errors
/// [`Stage0Error::ProbeUnavailable`] when the table records no config for the
/// chip, [`Stage0Error::WorkClock`] when the event will not open at all.
pub fn probe_work_clock(entry: &ChipEntry) -> Result<WorkClockProbe, Stage0Error> {
    let config = match entry.work_clock_config {
        TableValue::Recorded { value, .. } => value,
        TableValue::Absent { reason } => {
            return Err(Stage0Error::ProbeUnavailable {
                probe: format!("the work-clock event on {}", entry.name),
                reason: reason.to_string(),
            });
        }
    };

    // Host-user scope for the probe itself: the payload runs in this process, so
    // this is the only scope that can count it. Whether the guest-only scope
    // opens at all is a separate question, asked below.
    let counter = PerfCounter::open_counting(config, Scope::HostUser).map_err(|e| {
        Stage0Error::WorkClock {
            config,
            detail: e.to_string(),
        }
    })?;
    let spec = &payload::LOOP_BACKEDGE;

    counter
        .reset()
        .and_then(|()| counter.enable())
        .map_err(|e| Stage0Error::WorkClock {
            config,
            detail: e.to_string(),
        })?;
    let ran = payload::run(spec, WORK_CLOCK_PROBE_ITERATIONS);
    counter.disable().map_err(|e| Stage0Error::WorkClock {
        config,
        detail: e.to_string(),
    })?;
    if ran.is_none() {
        return Err(Stage0Error::ProbeUnavailable {
            probe: "the work-clock event probe".to_string(),
            reason: format!("payload {} did not run on this architecture", spec.name),
        });
    }
    let read = counter.read_timed().map_err(|e| Stage0Error::WorkClock {
        config,
        detail: e.to_string(),
    })?;

    Ok(WorkClockProbe {
        config,
        count: read.value,
        multiplexed: read.multiplexed(),
        guest_only_opened: PerfCounter::open_counting(config, Scope::GuestOnly).is_ok(),
    })
}

/// The row comparing the chip's performance-monitoring shape against the table.
///
/// # Errors
/// [`Stage0Error::Read`] when a source that should report the shape cannot be
/// read.
#[cfg(target_arch = "x86_64")]
pub fn pmu_shape_row(entry: &ChipEntry) -> Result<Option<Row>, Stage0Error> {
    let row = match entry.pmu_shape {
        PmuShape::IntelArchPerfmon { version } => Row::new(
            "pmu-shape",
            "host",
            format!("architectural performance monitoring version {version}"),
            format!(
                "architectural performance monitoring version {}",
                arch_perfmon_version()
            ),
        ),
        PmuShape::AmdCore => Row::new(
            "pmu-shape",
            "host",
            "AMD core performance monitoring",
            if amd_perfmon_v2() {
                "AMD core performance monitoring, PerfMonV2"
            } else {
                "AMD core performance monitoring"
            },
        ),
        PmuShape::ArmPmuV3 { .. } => return Ok(None),
    };
    Ok(Some(row))
}

/// The row comparing the chip's performance-monitoring shape against the table.
///
/// `PMCEID0_EL1` advertises the work-clock event and EL0 cannot read it, so on
/// aarch64 there is no shape to compare and the work-clock probe carries the
/// weight instead.
///
/// # Errors
/// Never on this architecture.
#[cfg(not(target_arch = "x86_64"))]
pub fn pmu_shape_row(_entry: &ChipEntry) -> Result<Option<Row>, Stage0Error> {
    Ok(None)
}

/// The architectural performance-monitoring version from CPUID leaf 0xA.
#[cfg(target_arch = "x86_64")]
fn arch_perfmon_version() -> u32 {
    core::arch::x86_64::__cpuid(0xA).eax & 0xff
}

/// Whether the chip advertises AMD PerfMonV2, from CPUID leaf 0x8000_0022.
#[cfg(target_arch = "x86_64")]
fn amd_perfmon_v2() -> bool {
    // Extended leaves above the reported maximum return the maximum leaf's
    // contents rather than zero, so the maximum is checked first.
    if core::arch::x86_64::__cpuid(0x8000_0000).eax < 0x8000_0022 {
        return false;
    }
    core::arch::x86_64::__cpuid(0x8000_0022).eax & 1 == 1
}

/// The rows proving AMD's speculative lock-mapping workaround is actually in
/// force, by the probe rr runs at startup: with the workaround in force, a
/// `lock add` commits no speculative lock map, so the counter reads zero.
///
/// Two rows, because one is not enough. A counter that reads zero because
/// nothing counted at all looks exactly like a counter that reads zero because
/// the workaround is in force, so the work clock counts the same payload run
/// and must read nonzero. Only then does the zero say something about the
/// workaround rather than about the performance-monitoring unit.
///
/// Returns nothing for chips whose entry names no such probe.
///
/// # Errors
/// [`Stage0Error::ProbeUnavailable`] when the entry names the probe but the
/// table records no event for it, [`Stage0Error::WorkClock`] when either event
/// will not open.
pub fn lock_probe_rows(entry: &ChipEntry) -> Result<Vec<Row>, Stage0Error> {
    let Some(event) = entry.lock_probe_event else {
        return Ok(Vec::new());
    };
    let config = match event {
        TableValue::Recorded { value, .. } => value,
        TableValue::Absent { reason } => {
            return Err(Stage0Error::ProbeUnavailable {
                probe: format!("the speculative lock-map probe on {}", entry.name),
                reason: reason.to_string(),
            });
        }
    };
    let work_config = match entry.work_clock_config {
        TableValue::Recorded { value, .. } => value,
        TableValue::Absent { reason } => {
            return Err(Stage0Error::ProbeUnavailable {
                probe: format!("the speculative lock-map probe's control on {}", entry.name),
                reason: reason.to_string(),
            });
        }
    };

    let open = |config: u64| -> Result<PerfCounter, Stage0Error> {
        PerfCounter::open_counting(config, Scope::HostUser).map_err(|e| Stage0Error::WorkClock {
            config,
            detail: e.to_string(),
        })
    };
    let start = |counter: &PerfCounter, config: u64| -> Result<(), Stage0Error> {
        counter
            .reset()
            .and_then(|()| counter.enable())
            .map_err(|e| Stage0Error::WorkClock {
                config,
                detail: e.to_string(),
            })
    };

    let spec = &payload::LOCKED;
    let commits = open(config)?;
    let control = open(work_config)?;
    start(&commits, config)?;
    start(&control, work_config)?;
    let ran = payload::run(spec, WORK_CLOCK_PROBE_ITERATIONS);
    for (counter, config) in [(&commits, config), (&control, work_config)] {
        counter.disable().map_err(|e| Stage0Error::WorkClock {
            config,
            detail: e.to_string(),
        })?;
    }
    if ran.is_none() {
        return Err(Stage0Error::ProbeUnavailable {
            probe: "the speculative lock-map probe".to_string(),
            reason: format!("payload {} did not run on this architecture", spec.name),
        });
    }
    let read = |counter: &PerfCounter, config: u64| -> Result<u64, Stage0Error> {
        counter
            .read_timed()
            .map(|r| r.value)
            .map_err(|e| Stage0Error::WorkClock {
                config,
                detail: e.to_string(),
            })
    };
    let commits_read = read(&commits, config)?;
    let control_read = read(&control, work_config)?;

    Ok(vec![
        Row::new(
            "spec-lock-map-commits",
            "host",
            "zero",
            if commits_read > 0 { "nonzero" } else { "zero" },
        ),
        Row::new(
            "spec-lock-map-probe-ran",
            "host",
            "nonzero",
            if control_read > 0 { "nonzero" } else { "zero" },
        ),
    ])
}

/// Run stage 0 against a pack: read the chip, match it, probe the work clock,
/// read every required host condition, and produce the rows.
///
/// # Errors
/// Any [`Stage0Error`] the reads, the match, or the comparison produce.
pub fn run(pack: &Pack) -> Result<Stage0Outcome, Stage0Error> {
    let chip = read_chip_identity()?;
    let entry = match_chip(&chip)?;
    let cpus = online_cpus()?;
    let work_clock = probe_work_clock(entry)?;
    let readings = read_conditions(entry, &cpus)?;
    let mut outcome = build_rows(entry, pack, &chip, &readings, &work_clock)?;
    outcome.add_rows(pmu_shape_row(entry)?);
    outcome.add_rows(lock_probe_rows(entry)?);
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_isolated_cpu_list_takes_singles_ranges_and_flag_words() {
        assert_eq!(isolated_cpus("ro isolcpus=3 quiet"), vec![3]);
        assert_eq!(isolated_cpus("isolcpus=1,3,5"), vec![1, 3, 5]);
        assert_eq!(
            isolated_cpus("isolcpus=domain,managed_irq,2-4"),
            vec![2, 3, 4]
        );
        assert!(isolated_cpus("ro quiet").is_empty());
    }

    #[test]
    fn the_vendor_module_is_read_before_the_shared_one() {
        assert_eq!(kvm_modules(Vendor::AuthenticAMD), &["kvm_amd", "kvm"]);
        assert_eq!(kvm_modules(Vendor::GenuineIntel), &["kvm_intel", "kvm"]);
        assert_eq!(kvm_modules(Vendor::Aarch64), &["kvm"]);
    }

    #[test]
    fn a_chip_with_no_lock_probe_produces_no_rows_for_one() {
        let intel = crate::chips::KNOWN_CHIPS
            .iter()
            .find(|e| e.vendor == Vendor::GenuineIntel)
            .expect("the table carries an Intel entry");
        assert!(
            lock_probe_rows(intel)
                .expect("no probe is not a refusal")
                .is_empty()
        );
    }
}
