// SPDX-License-Identifier: AGPL-3.0-or-later
//! The x86 CPU contract canonical serializer: emit the deterministic UTF-8 /
//! LF byte string [`super::contract_hash`] is taken over, from the parsed tables.
//!
//! **Rendering decisions (normative §6 + the spelling this implementation fixes;
//! documented because this serializer *defines* the v3 canonical bytes — the §6
//! registry is seeded from it).** Header records keep the literal §6 spelling
//! (decimal scalars; `mxcsr-mask=0x0000ffff` verbatim). Every record-body hex
//! number is **bare, lowercase, fixed-width** per §6's "N lowercase hex digits":
//! 8 for CPUID leaf/subleaf/register cells, 8 for an MSR index, 16 for a 64-bit
//! `allow-fixed` constant; `dyn:`/`emulate-*` formula ids and instruction tokens
//! are emitted verbatim (their meaning is hashed, not their definition text). The
//! emission order is the §6 item order: header, CPUID (sorted by leaf,subleaf, +
//! `cpuid-default zeroed`), MSR (one record per index, sorted), INSN (sorted by
//! mnemonic), timer (fixed device order), xAPIC MMIO (sorted by offset, +
//! `mmio-default`), CMOS (ports then indices, ranges expanded), and
//! guest invariants (fixed key order). Range/member rows
//! expand to one record per element before serialization. LF after every record.

use std::collections::BTreeMap;

use super::hex64;
use super::parse::{Contract, RegField, Subleaf};

/// Render an `(token, param)` disposition cell: `allow-fixed` carries a 16-hex
/// constant, every `emulate-*` token carries its formula id, all other tokens are
/// bare. (The hashed semantics of §6.)
fn cell(token: &str, param: Option<&str>) -> String {
    match (token, param) {
        ("allow-fixed", Some(p)) => format!("allow-fixed:{:016x}", hex64(p)),
        (t, Some(p)) if t.starts_with("emulate") => format!("{t}:{p}"),
        (t, _) => t.to_string(),
    }
}

/// Render a CPUID register field as its canonical token.
fn reg(field: RegField) -> String {
    match field {
        RegField::Const(v) => format!("{v:08x}"),
        RegField::DynOsxsave(b) => format!("dyn:osxsave:{b:08x}"),
        RegField::DynLevelEcho(t) => format!("dyn:level-echo:{t:08x}"),
        RegField::DynXcr0Xsavesize => "dyn:xcr0-xsavesize".to_string(),
    }
}

/// Render a CPUID subleaf token.
fn subleaf_tok(s: Subleaf) -> String {
    match s {
        Subleaf::Single(v) => format!("{v:08x}"),
        Subleaf::All => "*".to_string(),
        Subleaf::AndUp(n) => format!("{n:08x}+"),
        Subleaf::Range(lo, hi) => format!("{lo:08x}-{hi:08x}"),
    }
}

/// Emit the full §6 canonical form for `c`.
pub(crate) fn serialize(c: &Contract) -> String {
    let mut out = String::new();
    let mut line = |s: String| {
        out.push_str(&s);
        out.push('\n');
    };

    line(format!("contract-version={}", c.version));
    line(format!("kernel-tag={}", c.kernel_tag));
    line(format!("cpuid-baseline={}", c.cpuid_baseline));
    line(format!("tsc-hz={}", c.tsc_hz));
    line(format!("crystal-hz={}", c.crystal_hz));
    line(format!("bus-hz={}", c.bus_hz));
    line(format!("mxcsr-mask={}", c.mxcsr_mask));
    line(format!("rtc-epoch={}", c.rtc_epoch));
    line(format!("pit-refresh-ns={}", c.pit_refresh_ns));
    line(format!(
        "vtime-interrupt-controller-vns={}",
        c.vtime_interrupt_controller_vns
    ));
    line(format!("vtime-serial-vns={}", c.vtime_serial_vns));
    line(format!("vtime-paravirtual-vns={}", c.vtime_paravirtual_vns));
    line(format!("vtime-time-read-vns={}", c.vtime_time_read_vns));
    line(format!(
        "vtime-arch-control-vns={}",
        c.vtime_arch_control_vns
    ));
    line(format!(
        "vtime-execution-tick-vns={}",
        c.vtime_execution_tick_vns
    ));
    line(format!(
        "vtime-clockevent-period-vns={}",
        c.vtime_clockevent_period_vns
    ));

    let mut cpuid: Vec<_> = c.cpuid.clone();
    cpuid.sort_by_key(|r| (r.leaf.lo, subleaf_sort_key(r.subleaf)));
    for r in &cpuid {
        let leaf = if r.leaf.lo == r.leaf.hi {
            format!("{:08x}", r.leaf.lo)
        } else {
            format!("{:08x}-{:08x}", r.leaf.lo, r.leaf.hi)
        };
        line(format!(
            "cpuid {leaf}.{} {} {} {} {}",
            subleaf_tok(r.subleaf),
            reg(r.eax),
            reg(r.ebx),
            reg(r.ecx),
            reg(r.edx),
        ));
    }
    line("cpuid-default zeroed".to_string());

    let mut msr: BTreeMap<u32, (String, String)> = BTreeMap::new();
    for row in &c.msr {
        let read = cell(&row.read, row.read_param.as_deref());
        let write = cell(&row.write, row.write_param.as_deref());
        for idx in row.index.indices() {
            msr.insert(idx, (read.clone(), write.clone()));
        }
    }
    for (idx, (read, write)) in &msr {
        line(format!("msr {idx:08x} {read} {write}"));
    }

    let mut insn: Vec<_> = c.insn.clone();
    insn.sort_by(|a, b| a.mnemonic.cmp(&b.mnemonic));
    for r in &insn {
        line(format!(
            "insn {} {} {} {}",
            r.mnemonic, r.mechanism, r.result, r.determinism
        ));
    }

    for r in &c.timer {
        line(format!(
            "timer {} {} {}",
            r.device,
            cell(&r.read, r.read_param.as_deref()),
            cell(&r.write, r.write_param.as_deref()),
        ));
    }

    let mut mmio: Vec<_> = c.mmio.clone();
    mmio.sort_by_key(|r| u32::from_str_radix(&r.offset, 16).unwrap_or(0));
    for r in &mmio {
        line(format!(
            "mmio xapic.{} {} {}",
            r.offset,
            cell(&r.read, r.read_param.as_deref()),
            cell(&r.write, r.write_param.as_deref()),
        ));
    }
    line(format!(
        "mmio-default {} {}",
        cell(&c.mmio_default_read, c.mmio_default_read_param.as_deref()),
        cell(&c.mmio_default_write, c.mmio_default_write_param.as_deref()),
    ));

    let mut cmos: Vec<(u8, u32, String, String, String)> = Vec::new();
    for r in &c.cmos {
        let read = cell(&r.read, r.read_param.as_deref());
        let write = cell(&r.write, r.write_param.as_deref());
        if let Some(p) = r.where_.strip_prefix("port:0x") {
            let v = u32::from_str_radix(p, 16).unwrap_or(0);
            cmos.push((0, v, r.where_.clone(), read, write));
        } else if let Some(p) = r.where_.strip_prefix("idx:0x") {
            if let Some((lo, hi)) = p.split_once("-0x") {
                let (lo, hi) = (
                    u32::from_str_radix(lo, 16).unwrap_or(0),
                    u32::from_str_radix(hi, 16).unwrap_or(0),
                );
                for i in lo..=hi {
                    cmos.push((1, i, format!("idx:0x{i:02x}"), read.clone(), write.clone()));
                }
            } else {
                let v = u32::from_str_radix(p, 16).unwrap_or(0);
                cmos.push((1, v, r.where_.clone(), read, write));
            }
        }
    }
    cmos.sort_by_key(|(g, v, _, _, _)| (*g, *v));
    for (_, _, where_, read, write) in &cmos {
        line(format!("cmos {where_} {read} {write}"));
    }

    line(format!("guest ucode-rev {}", c.guest_ucode_rev));
    line(format!(
        "guest cr4-force-reserved [{}]",
        c.cr4_force_reserved.join(", ")
    ));

    out
}

/// Sort key for a subleaf token (lowest covered subleaf).
fn subleaf_sort_key(s: Subleaf) -> u32 {
    match s {
        Subleaf::Single(v) | Subleaf::AndUp(v) | Subleaf::Range(v, _) => v,
        Subleaf::All => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_renders_each_disposition_shape() {
        assert_eq!(
            cell("allow-fixed", Some("0xfee00900")),
            "allow-fixed:00000000fee00900"
        );
        assert_eq!(
            cell("emulate-vtime", Some("vclock.tsc")),
            "emulate-vtime:vclock.tsc"
        );
        assert_eq!(
            cell("emulate-device", Some("pit.ch0")),
            "emulate-device:pit.ch0"
        );
        assert_eq!(cell("deny-gp", Some("foo")), "deny-gp");
        assert_eq!(cell("allow-stateful", Some("bar")), "allow-stateful");
        assert_eq!(cell("deny-ignore-write", None), "deny-ignore-write");
    }

    #[test]
    fn subleaf_sort_key_is_the_lowest_covered_subleaf() {
        assert_eq!(subleaf_sort_key(Subleaf::Single(5)), 5);
        assert_eq!(subleaf_sort_key(Subleaf::AndUp(3)), 3);
        assert_eq!(subleaf_sort_key(Subleaf::Range(7, 9)), 7);
        assert_eq!(subleaf_sort_key(Subleaf::All), 0);
    }

    #[test]
    fn reg_and_subleaf_tokens_render() {
        assert_eq!(reg(RegField::Const(0xABCD)), "0000abcd");
        assert_eq!(
            reg(RegField::DynOsxsave(0x76da3203)),
            "dyn:osxsave:76da3203"
        );
        assert_eq!(reg(RegField::DynLevelEcho(0x2)), "dyn:level-echo:00000002");
        assert_eq!(reg(RegField::DynXcr0Xsavesize), "dyn:xcr0-xsavesize");
        assert_eq!(subleaf_tok(Subleaf::Single(0)), "00000000");
        assert_eq!(subleaf_tok(Subleaf::All), "*");
        assert_eq!(subleaf_tok(Subleaf::AndUp(4)), "00000004+");
        assert_eq!(subleaf_tok(Subleaf::Range(1, 3)), "00000001-00000003");
    }
}
