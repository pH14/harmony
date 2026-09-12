// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VendorId {
    GenuineIntel,
    AuthenticAMD,
}

impl VendorId {
    pub(crate) const fn cpuid_string(self) -> &'static str {
        match self {
            VendorId::GenuineIntel => "GenuineIntel",
            VendorId::AuthenticAMD => "AuthenticAMD",
        }
    }

    fn as_token(self) -> &'static str {
        match self {
            VendorId::GenuineIntel => "GenuineIntel",
            VendorId::AuthenticAMD => "AuthenticAMD",
        }
    }

    fn from_token(s: &str) -> Option<VendorId> {
        match s {
            "GenuineIntel" => Some(VendorId::GenuineIntel),
            "AuthenticAMD" => Some(VendorId::AuthenticAMD),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum ContractError {
    #[error("contract vendor mismatch: file declares {found}, loaded under {expected}")]
    VendorMismatch {
        expected: &'static str,
        found: String,
    },
    #[error("mixed-vendor artifact: declares vendor {declared}, but CPUID leaf 0 spells {leaf0}")]
    MixedVendor {
        declared: &'static str,
        leaf0: String,
    },
    #[error("unknown contract vendor token {token:?} (expected GenuineIntel or AuthenticAMD)")]
    UnknownVendor { token: String },
    #[error(
        "malformed CPUID leaf 0 under vendor {declared}: expected exactly one all-constant single (0,0) row spelling a UTF-8 vendor string"
    )]
    MalformedLeaf0 { declared: &'static str },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TomlValue {
    Str(String),
    Int(i64),
    Bool(bool),
    Arr(Vec<String>),
}

impl TomlValue {
    fn as_str(&self) -> &str {
        match self {
            TomlValue::Str(s) => s,
            _ => "",
        }
    }
    fn as_int(&self) -> i64 {
        match self {
            TomlValue::Int(i) => *i,
            _ => 0,
        }
    }
    #[cfg(test)]
    fn as_bool(&self) -> bool {
        matches!(self, TomlValue::Bool(true))
    }
    fn as_arr(&self) -> &[String] {
        match self {
            TomlValue::Arr(v) => v,
            _ => &[],
        }
    }
}

struct Raw {
    singletons: BTreeMap<String, BTreeMap<String, TomlValue>>,
    arrays: BTreeMap<String, Vec<BTreeMap<String, TomlValue>>>,
}

fn strip_comment(line: &str) -> &str {
    let mut in_str = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_str = !in_str,
            '#' if !in_str => return &line[..i],
            _ => {}
        }
    }
    line
}

fn parse_value(s: &str) -> TomlValue {
    let s = s.trim();
    if let Some(inner) = s.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
        let items = inner
            .split(',')
            .map(|x| x.trim().trim_matches('"').to_string())
            .filter(|x| !x.is_empty())
            .collect();
        TomlValue::Arr(items)
    } else if s.starts_with('"') {
        TomlValue::Str(s.trim_matches('"').to_string())
    } else if s == "true" || s == "false" {
        TomlValue::Bool(s == "true")
    } else {
        TomlValue::Int(s.parse().unwrap_or(0))
    }
}

enum Target {
    None,
    Singleton(String),
    Array(String),
}

fn parse_raw(toml: &str) -> Raw {
    let mut raw = Raw {
        singletons: BTreeMap::new(),
        arrays: BTreeMap::new(),
    };
    let mut target = Target::None;

    for line in toml.lines() {
        let line = strip_comment(line).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix("[[").and_then(|x| x.strip_suffix("]]")) {
            let name = name.to_string();
            raw.arrays
                .entry(name.clone())
                .or_default()
                .push(BTreeMap::new());
            target = Target::Array(name);
        } else if let Some(name) = line.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
            let name = name.to_string();
            raw.singletons.entry(name.clone()).or_default();
            target = Target::Singleton(name);
        } else if let Some((k, v)) = line.split_once('=') {
            let (k, v) = (k.trim().to_string(), parse_value(v));
            match &target {
                Target::Singleton(name) => {
                    raw.singletons.entry(name.clone()).or_default().insert(k, v);
                }
                Target::Array(name) => {
                    if let Some(last) = raw.arrays.get_mut(name).and_then(|a| a.last_mut()) {
                        last.insert(k, v);
                    }
                }
                Target::None => {}
            }
        }
    }
    raw
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct LeafSpec {
    pub lo: u32,
    pub hi: u32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Subleaf {
    Single(u32),
    All,
    AndUp(u32),
    Range(u32, u32),
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum RegField {
    Const(u32),
    DynOsxsave(u32),
    DynLevelEcho(u32),
    DynXcr0Xsavesize,
}

impl RegField {
    pub(crate) fn base(self) -> u32 {
        match self {
            RegField::Const(v) | RegField::DynOsxsave(v) => v,
            RegField::DynLevelEcho(t) => t << 8,
            RegField::DynXcr0Xsavesize => 0x340,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CpuidRow {
    pub leaf: LeafSpec,
    pub subleaf: Subleaf,
    pub eax: RegField,
    pub ebx: RegField,
    pub ecx: RegField,
    pub edx: RegField,
}

#[derive(Clone, Debug)]
pub(crate) enum IndexSpec {
    Single(u32),
    Range(u32, u32),
    Members(Vec<u32>),
}

impl IndexSpec {
    pub(crate) fn indices(&self) -> Vec<u32> {
        match self {
            IndexSpec::Single(i) => vec![*i],
            IndexSpec::Range(lo, hi) => (*lo..=*hi).collect(),
            IndexSpec::Members(m) => {
                let mut v = m.clone();
                v.sort_unstable();
                v
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct MsrRow {
    pub index: IndexSpec,
    pub read: String,
    pub read_param: Option<String>,
    pub write: String,
    pub write_param: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct InsnRow {
    pub mnemonic: String,
    pub mechanism: String,
    pub result: String,
    pub determinism: String,
}

#[derive(Clone, Debug)]
pub(crate) struct TimerRow {
    pub device: String,
    pub read: String,
    pub read_param: Option<String>,
    pub write: String,
    pub write_param: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct CmosRow {
    pub where_: String,
    pub read: String,
    pub read_param: Option<String>,
    pub write: String,
    pub write_param: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct MmioRow {
    pub offset: String,
    pub read: String,
    pub read_param: Option<String>,
    pub write: String,
    pub write_param: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct Contract {
    pub vendor: VendorId,
    pub vendor_declared: Option<String>,
    pub version: i64,
    pub kernel_tag: String,
    pub cpuid_baseline: String,
    pub tsc_hz: i64,
    pub crystal_hz: i64,
    pub bus_hz: i64,
    pub mxcsr_mask: String,
    pub rtc_epoch: i64,
    pub pit_refresh_ns: i64,
    pub vtime_interrupt_controller_vns: i64,
    pub vtime_serial_vns: i64,
    pub vtime_paravirtual_vns: i64,
    pub vtime_time_read_vns: i64,
    pub vtime_arch_control_vns: i64,
    pub vtime_execution_tick_vns: i64,
    pub vtime_clockevent_period_vns: i64,
    #[cfg(test)]
    pub contract_hash: Option<String>,
    pub cpuid: Vec<CpuidRow>,
    pub msr: Vec<MsrRow>,
    pub insn: Vec<InsnRow>,
    pub timer: Vec<TimerRow>,
    pub cmos: Vec<CmosRow>,
    pub mmio_default_read: String,
    pub mmio_default_read_param: Option<String>,
    pub mmio_default_write: String,
    pub mmio_default_write_param: Option<String>,
    pub mmio: Vec<MmioRow>,
    pub guest_ucode_rev: String,
    pub cr4_force_reserved: Vec<String>,
}

fn hex32(s: &str) -> u32 {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x") {
        u32::from_str_radix(h, 16).expect("contract: malformed 32-bit hex")
    } else {
        s.parse().expect("contract: malformed 32-bit decimal")
    }
}

fn reg_field(s: &str) -> RegField {
    if let Some(rest) = s.strip_prefix("dyn:") {
        if let Some(base) = rest.strip_prefix("osxsave:") {
            RegField::DynOsxsave(hex32(base))
        } else if let Some(t) = rest.strip_prefix("level-echo:") {
            RegField::DynLevelEcho(hex32(t))
        } else if rest == "xcr0-xsavesize" {
            RegField::DynXcr0Xsavesize
        } else {
            panic!("contract: unknown dyn cpuid token: {s}")
        }
    } else {
        RegField::Const(hex32(s))
    }
}

fn subleaf(s: &str) -> Subleaf {
    if s == "*" {
        Subleaf::All
    } else if let Some(n) = s.strip_suffix('+') {
        Subleaf::AndUp(hex32(n))
    } else if let Some((lo, hi)) = s.split_once('-') {
        Subleaf::Range(hex32(lo), hex32(hi))
    } else {
        Subleaf::Single(hex32(s))
    }
}

fn index_spec_of(e: &BTreeMap<String, TomlValue>) -> IndexSpec {
    if let Some(members) = e.get("index-members") {
        IndexSpec::Members(members.as_arr().iter().map(|s| hex32(s)).collect())
    } else if let Some(lo) = e.get("index-lo") {
        IndexSpec::Range(hex32(lo.as_str()), hex32(e["index-hi"].as_str()))
    } else {
        IndexSpec::Single(hex32(e["index"].as_str()))
    }
}

fn dispositions(
    e: &BTreeMap<String, TomlValue>,
) -> (String, Option<String>, String, Option<String>) {
    let param = |k: &str| e.get(k).map(|v| v.as_str().to_string());
    (
        e.get("read")
            .map(|v| v.as_str().to_string())
            .unwrap_or_default(),
        param("read-param"),
        e.get("write")
            .map(|v| v.as_str().to_string())
            .unwrap_or_default(),
        param("write-param"),
    )
}

fn canonical_leaf0_vendor_string(covering: &[&CpuidRow]) -> Option<String> {
    let [row] = covering else {
        return None;
    };
    if !(row.leaf.lo == 0 && row.leaf.hi == 0 && matches!(row.subleaf, Subleaf::Single(0))) {
        return None;
    }
    let (RegField::Const(_eax), RegField::Const(ebx), RegField::Const(ecx), RegField::Const(edx)) =
        (row.eax, row.ebx, row.ecx, row.edx)
    else {
        return None;
    };
    let mut bytes = Vec::with_capacity(12);
    for reg in [ebx, edx, ecx] {
        bytes.extend_from_slice(&reg.to_le_bytes());
    }
    String::from_utf8(bytes).ok()
}

fn covers_leaf0_subleaf0(row: &CpuidRow) -> bool {
    let leaf_covers = row.leaf.lo == 0;
    let subleaf_covers = match row.subleaf {
        Subleaf::Single(v) => v == 0,
        Subleaf::All => true,
        Subleaf::AndUp(lo) => lo == 0,
        Subleaf::Range(lo, _) => lo == 0,
    };
    leaf_covers && subleaf_covers
}

impl Contract {
    pub(crate) fn parse(toml: &str) -> Contract {
        let raw = parse_raw(toml);
        let empty = BTreeMap::new();
        let c = raw.singletons.get("contract").unwrap_or(&empty);
        let mmio = raw.singletons.get("mmio").unwrap_or(&empty);
        let guest = raw.singletons.get("guest").unwrap_or(&empty);

        let cpuid = raw
            .arrays
            .get("cpuid.entry")
            .map(|rows| rows.iter().map(Self::cpuid_row).collect())
            .unwrap_or_default();
        let msr = raw
            .arrays
            .get("msr.entry")
            .map(|rows| rows.iter().map(Self::msr_row).collect())
            .unwrap_or_default();
        let insn = raw
            .arrays
            .get("insn.entry")
            .map(|rows| {
                rows.iter()
                    .map(|e| InsnRow {
                        mnemonic: e["mnemonic"].as_str().to_string(),
                        mechanism: e["mechanism"].as_str().to_string(),
                        result: e["result"].as_str().to_string(),
                        determinism: e["determinism"].as_str().to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let timer = raw
            .arrays
            .get("timer.entry")
            .map(|rows| {
                rows.iter()
                    .map(|e| {
                        let (read, read_param, write, write_param) = dispositions(e);
                        TimerRow {
                            device: e["device"].as_str().to_string(),
                            read,
                            read_param,
                            write,
                            write_param,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let cmos = raw
            .arrays
            .get("cmos.entry")
            .map(|rows| {
                rows.iter()
                    .map(|e| {
                        let (read, read_param, write, write_param) = dispositions(e);
                        CmosRow {
                            where_: e["where"].as_str().to_string(),
                            read,
                            read_param,
                            write,
                            write_param,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mmio_rows = raw
            .arrays
            .get("mmio.entry")
            .map(|rows| {
                rows.iter()
                    .map(|e| {
                        let (read, read_param, write, write_param) = dispositions(e);
                        MmioRow {
                            offset: e["offset"].as_str().to_string(),
                            read,
                            read_param,
                            write,
                            write_param,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        let vendor_declared = c.get("vendor").map(|v| v.as_str().to_string());
        let vendor = vendor_declared
            .as_deref()
            .and_then(VendorId::from_token)
            .unwrap_or(VendorId::GenuineIntel);

        Contract {
            vendor,
            vendor_declared,
            version: c.get("version").map(TomlValue::as_int).unwrap_or_default(),
            kernel_tag: c
                .get("kernel-tag")
                .map(|v| v.as_str().to_string())
                .unwrap_or_default(),
            cpuid_baseline: c
                .get("cpuid-baseline")
                .map(|v| v.as_str().to_string())
                .unwrap_or_default(),
            tsc_hz: c.get("tsc-hz").map(TomlValue::as_int).unwrap_or_default(),
            crystal_hz: c
                .get("crystal-hz")
                .map(TomlValue::as_int)
                .unwrap_or_default(),
            bus_hz: c.get("bus-hz").map(TomlValue::as_int).unwrap_or_default(),
            mxcsr_mask: c
                .get("mxcsr-mask")
                .map(|v| v.as_str().to_string())
                .unwrap_or_default(),
            rtc_epoch: c
                .get("rtc-epoch")
                .map(TomlValue::as_int)
                .unwrap_or_default(),
            pit_refresh_ns: c
                .get("pit-refresh-ns")
                .map(TomlValue::as_int)
                .unwrap_or_default(),
            vtime_interrupt_controller_vns: c
                .get("vtime-interrupt-controller-vns")
                .map(TomlValue::as_int)
                .unwrap_or_default(),
            vtime_serial_vns: c
                .get("vtime-serial-vns")
                .map(TomlValue::as_int)
                .unwrap_or_default(),
            vtime_paravirtual_vns: c
                .get("vtime-paravirtual-vns")
                .map(TomlValue::as_int)
                .unwrap_or_default(),
            vtime_time_read_vns: c
                .get("vtime-time-read-vns")
                .map(TomlValue::as_int)
                .unwrap_or_default(),
            vtime_arch_control_vns: c
                .get("vtime-arch-control-vns")
                .map(TomlValue::as_int)
                .unwrap_or_default(),
            vtime_execution_tick_vns: c
                .get("vtime-execution-tick-vns")
                .map(TomlValue::as_int)
                .unwrap_or_default(),
            vtime_clockevent_period_vns: c
                .get("vtime-clockevent-period-vns")
                .map(TomlValue::as_int)
                .unwrap_or_default(),
            #[cfg(test)]
            contract_hash: c.get("contract_hash").map(|v| v.as_str().to_string()),
            cpuid,
            msr,
            insn,
            timer,
            cmos,
            mmio_default_read: mmio
                .get("default-read")
                .map(|v| v.as_str().to_string())
                .unwrap_or_default(),
            mmio_default_read_param: mmio
                .get("default-read-param")
                .map(|v| v.as_str().to_string()),
            mmio_default_write: mmio
                .get("default-write")
                .map(|v| v.as_str().to_string())
                .unwrap_or_default(),
            mmio_default_write_param: mmio
                .get("default-write-param")
                .map(|v| v.as_str().to_string()),
            mmio: mmio_rows,
            guest_ucode_rev: guest
                .get("ucode-rev")
                .map(|v| v.as_str().to_string())
                .unwrap_or_default(),
            cr4_force_reserved: guest
                .get("cr4-force-reserved")
                .map(|v| v.as_arr().to_vec())
                .unwrap_or_default(),
        }
    }

    fn cpuid_row(e: &BTreeMap<String, TomlValue>) -> CpuidRow {
        let leaf = if let Some(lo) = e.get("leaf-lo") {
            LeafSpec {
                lo: hex32(lo.as_str()),
                hi: hex32(e["leaf-hi"].as_str()),
            }
        } else {
            let v = hex32(e["leaf"].as_str());
            LeafSpec { lo: v, hi: v }
        };
        CpuidRow {
            leaf,
            subleaf: subleaf(e["subleaf"].as_str()),
            eax: reg_field(e["eax"].as_str()),
            ebx: reg_field(e["ebx"].as_str()),
            ecx: reg_field(e["ecx"].as_str()),
            edx: reg_field(e["edx"].as_str()),
        }
    }

    fn msr_row(e: &BTreeMap<String, TomlValue>) -> MsrRow {
        let index = index_spec_of(e);
        let (read, read_param, write, write_param) = dispositions(e);
        MsrRow {
            index,
            read,
            read_param,
            write,
            write_param,
        }
    }

    pub(crate) fn load(toml: &str, expected: VendorId) -> Result<Contract, ContractError> {
        let c = Self::parse(toml);
        if let Some(tok) = c.vendor_declared.as_deref()
            && VendorId::from_token(tok).is_none()
        {
            return Err(ContractError::UnknownVendor {
                token: tok.to_string(),
            });
        }
        if c.vendor != expected {
            return Err(ContractError::VendorMismatch {
                expected: expected.as_token(),
                found: c.vendor.as_token().to_string(),
            });
        }
        let covering: Vec<&CpuidRow> = c
            .cpuid
            .iter()
            .filter(|r| covers_leaf0_subleaf0(r))
            .collect();
        if !covering.is_empty() {
            let leaf0 =
                canonical_leaf0_vendor_string(&covering).ok_or(ContractError::MalformedLeaf0 {
                    declared: expected.as_token(),
                })?;
            if leaf0 != expected.cpuid_string() {
                return Err(ContractError::MixedVendor {
                    declared: expected.as_token(),
                    leaf0,
                });
            }
        }
        Ok(c)
    }

    pub(crate) fn leaf_entry_count(&self, leaf_lo: u32) -> usize {
        self.cpuid.iter().filter(|r| r.leaf.lo == leaf_lo).count()
    }
}

#[cfg(test)]
mod tests {

    use proptest::prelude::*;

    use super::*;

    #[test]
    fn toml_value_accessors_and_fallbacks() {
        assert_eq!(TomlValue::Str("x".into()).as_str(), "x");
        assert_eq!(TomlValue::Int(7).as_int(), 7);
        assert!(TomlValue::Bool(true).as_bool());
        assert!(!TomlValue::Bool(false).as_bool());
        assert_eq!(
            TomlValue::Arr(vec!["a".into(), "b".into()]).as_arr(),
            ["a", "b"]
        );
        assert_eq!(TomlValue::Int(1).as_str(), "");
        assert_eq!(TomlValue::Str("x".into()).as_int(), 0);
        assert!(!TomlValue::Int(1).as_bool());
        assert!(!TomlValue::Str("true".into()).as_bool());
        assert_eq!(TomlValue::Int(1).as_arr(), &[] as &[String]);
    }

    #[test]
    fn strip_comment_respects_quoted_hashes() {
        assert_eq!(strip_comment("key = 1 # trailing"), "key = 1 ");
        assert_eq!(strip_comment("# whole line"), "");
        assert_eq!(strip_comment("no comment here"), "no comment here");
        assert_eq!(strip_comment("k = \"a#b\""), "k = \"a#b\"");
        assert_eq!(strip_comment("k = \"v\" # c"), "k = \"v\" ");
    }

    #[test]
    fn parse_value_classifies_every_token() {
        assert_eq!(parse_value("true"), TomlValue::Bool(true));
        assert_eq!(parse_value("false"), TomlValue::Bool(false));
        assert_eq!(parse_value("  true  "), TomlValue::Bool(true));
        assert_eq!(parse_value("46"), TomlValue::Int(46));
        assert_eq!(parse_value("0"), TomlValue::Int(0));
        assert_eq!(parse_value("not_a_number"), TomlValue::Int(0));
        assert_eq!(parse_value("\"hello\""), TomlValue::Str("hello".into()));
        assert_eq!(parse_value("\"\""), TomlValue::Str(String::new()));
        assert_eq!(
            parse_value("[\"a\", \"b\"]"),
            TomlValue::Arr(vec!["a".into(), "b".into()])
        );
        assert_eq!(parse_value("[]"), TomlValue::Arr(vec![]));
        assert_eq!(
            parse_value("[\"a\", \"\", \"b\", ]"),
            TomlValue::Arr(vec!["a".into(), "b".into()])
        );
    }

    #[test]
    fn parse_raw_sections_arrays_and_stray_keys() {
        let raw = parse_raw(
            "stray = 1\n\
             # comment line\n\
             [contract]\n\
             version = 2\n\
             [[cpuid.entry]]\n\
             leaf = \"0x1\"\n\
             [[cpuid.entry]]\n\
             leaf = \"0x2\"\n",
        );
        assert!(!raw.singletons.values().any(|m| m.contains_key("stray")));
        assert_eq!(
            raw.singletons.get("contract").unwrap().get("version"),
            Some(&TomlValue::Int(2))
        );
        assert_eq!(raw.arrays.get("cpuid.entry").unwrap().len(), 2);
    }

    #[test]
    fn hex32_parses_hex_and_decimal() {
        assert_eq!(hex32("0x100000"), 0x10_0000);
        assert_eq!(hex32("0x0"), 0);
        assert_eq!(hex32("46"), 46);
        assert_eq!(hex32("  0x1b  "), 0x1b);
    }

    #[test]
    fn reg_field_parses_const_and_dyn_tokens() {
        assert!(matches!(
            reg_field("0x12345678"),
            RegField::Const(0x1234_5678)
        ));
        assert!(matches!(
            reg_field("dyn:osxsave:0x76da3203"),
            RegField::DynOsxsave(0x76da_3203)
        ));
        assert!(matches!(
            reg_field("dyn:level-echo:0x2"),
            RegField::DynLevelEcho(2)
        ));
        assert!(matches!(
            reg_field("dyn:xcr0-xsavesize"),
            RegField::DynXcr0Xsavesize
        ));
    }

    #[test]
    fn subleaf_parses_all_forms() {
        assert!(matches!(subleaf("0x0"), Subleaf::Single(0)));
        assert!(matches!(subleaf("0x5"), Subleaf::Single(5)));
        assert!(matches!(subleaf("*"), Subleaf::All));
        assert!(matches!(subleaf("0x2+"), Subleaf::AndUp(2)));
        assert!(matches!(subleaf("0x1-0x3"), Subleaf::Range(1, 3)));
    }

    #[test]
    fn reg_field_base_is_exact() {
        assert_eq!(RegField::Const(0xDEAD_BEEF).base(), 0xDEAD_BEEF);
        assert_eq!(RegField::DynOsxsave(0x76da_3203).base(), 0x76da_3203);
        assert_eq!(RegField::DynLevelEcho(0x01).base(), 0x0100);
        assert_eq!(RegField::DynLevelEcho(0x12).base(), 0x1200);
        assert_eq!(RegField::DynXcr0Xsavesize.base(), 0x340);
    }

    #[test]
    fn index_spec_indices_expands_and_sorts() {
        assert_eq!(IndexSpec::Single(0x10).indices(), vec![0x10]);
        assert_eq!(
            IndexSpec::Range(0x800, 0x803).indices(),
            vec![0x800, 0x801, 0x802, 0x803]
        );
        assert_eq!(
            IndexSpec::Members(vec![0x30, 0x10, 0x20]).indices(),
            vec![0x10, 0x20, 0x30]
        );
    }

    #[test]
    fn dispositions_reads_tokens_and_optional_params() {
        let mut e = BTreeMap::new();
        e.insert("read".to_string(), TomlValue::Str("allow-fixed".into()));
        e.insert("read-param".to_string(), TomlValue::Str("0x10".into()));
        e.insert("write".to_string(), TomlValue::Str("deny-gp".into()));
        let (r, rp, w, wp) = dispositions(&e);
        assert_eq!(
            (r.as_str(), rp.as_deref(), w.as_str(), wp),
            ("allow-fixed", Some("0x10"), "deny-gp", None)
        );

        let (r2, rp2, w2, wp2) = dispositions(&BTreeMap::new());
        assert_eq!((r2.as_str(), rp2, w2.as_str(), wp2), ("", None, "", None));
    }

    fn entry(pairs: &[(&str, TomlValue)]) -> BTreeMap<String, TomlValue> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn cpuid_row_single_and_range_leaf() {
        let single = Contract::cpuid_row(&entry(&[
            ("leaf", TomlValue::Str("0x1".into())),
            ("subleaf", TomlValue::Str("0x0".into())),
            ("eax", TomlValue::Str("0x1".into())),
            ("ebx", TomlValue::Str("dyn:osxsave:0x10".into())),
            ("ecx", TomlValue::Str("0x3".into())),
            ("edx", TomlValue::Str("0x4".into())),
        ]));
        assert_eq!((single.leaf.lo, single.leaf.hi), (1, 1));
        assert!(matches!(single.ebx, RegField::DynOsxsave(0x10)));

        let range = Contract::cpuid_row(&entry(&[
            ("leaf-lo", TomlValue::Str("0x40000000".into())),
            ("leaf-hi", TomlValue::Str("0x400000ff".into())),
            ("subleaf", TomlValue::Str("*".into())),
            ("eax", TomlValue::Str("0x0".into())),
            ("ebx", TomlValue::Str("0x0".into())),
            ("ecx", TomlValue::Str("0x0".into())),
            ("edx", TomlValue::Str("0x0".into())),
        ]));
        assert_eq!((range.leaf.lo, range.leaf.hi), (0x4000_0000, 0x4000_00ff));
        assert!(matches!(range.subleaf, Subleaf::All));
    }

    #[test]
    fn msr_row_single_range_and_members() {
        let single = Contract::msr_row(&entry(&[
            ("index", TomlValue::Str("0x10".into())),
            ("read", TomlValue::Str("emulate-vtime".into())),
            ("read-param", TomlValue::Str("vclock.tsc".into())),
            ("write", TomlValue::Str("deny-gp".into())),
        ]));
        assert_eq!(single.index.indices(), vec![0x10]);
        assert_eq!(single.read_param.as_deref(), Some("vclock.tsc"));

        let range = Contract::msr_row(&entry(&[
            ("index-lo", TomlValue::Str("0x800".into())),
            ("index-hi", TomlValue::Str("0x802".into())),
            ("read", TomlValue::Str("deny-gp".into())),
            ("write", TomlValue::Str("deny-gp".into())),
        ]));
        assert_eq!(range.index.indices(), vec![0x800, 0x801, 0x802]);

        let members = Contract::msr_row(&entry(&[
            (
                "index-members",
                TomlValue::Arr(vec!["0x20".into(), "0x10".into()]),
            ),
            ("read", TomlValue::Str("deny-gp".into())),
            ("write", TomlValue::Str("deny-gp".into())),
        ]));
        assert_eq!(members.index.indices(), vec![0x10, 0x20]);
    }

    const SYNTH: &str = "\
# leading comment\n\
[contract]\n\
version = 2\n\
kernel-tag = \"v6.18.35\"\n\
cpuid-baseline = \"test-baseline\"\n\
tsc-hz = 2000000000  # inline comment after a value\n\
mxcsr-mask = \"0x0000ffff\"\n\
\n\
[[cpuid.entry]]\n\
leaf = \"0x1\"\n\
subleaf = \"0x0\"\n\
eax = \"0x1\"\n\
ebx = \"0x2\"\n\
ecx = \"dyn:osxsave:0x76da3203\"\n\
edx = \"0x4\"\n\
[[cpuid.entry]]\n\
leaf = \"0x1\"\n\
subleaf = \"0x1\"\n\
eax = \"0x0\"\n\
ebx = \"0x0\"\n\
ecx = \"0x0\"\n\
edx = \"0x0\"\n\
[[cpuid.entry]]\n\
leaf = \"0x4\"\n\
subleaf = \"0x0\"\n\
eax = \"0x0\"\n\
ebx = \"0x0\"\n\
ecx = \"0x0\"\n\
edx = \"0x0\"\n\
\n\
[[msr.entry]]\n\
index = \"0x10\"\n\
read = \"emulate-vtime\"\n\
read-param = \"vclock.tsc\"\n\
write = \"emulate-vtime\"\n\
write-param = \"vclock.tsc.write\"\n\
\n\
[guest]\n\
cr4-force-reserved = [\"PKE\", \"PKS\"]\n";

    #[test]
    fn parse_full_synthetic_contract() {
        let c = Contract::parse(SYNTH);
        assert_eq!(c.version, 2);
        assert_eq!(c.kernel_tag, "v6.18.35");
        assert_eq!(c.cpuid_baseline, "test-baseline");
        assert_eq!(c.tsc_hz, 2_000_000_000);
        assert_eq!(c.mxcsr_mask, "0x0000ffff");
        assert_eq!(
            c.cr4_force_reserved,
            vec!["PKE".to_string(), "PKS".to_string()]
        );

        assert_eq!(c.cpuid.len(), 3);
        assert!(matches!(c.cpuid[0].ecx, RegField::DynOsxsave(0x76da_3203)));
        assert_eq!(c.msr.len(), 1);
        assert_eq!(c.msr[0].read, "emulate-vtime");
        assert_eq!(c.msr[0].read_param.as_deref(), Some("vclock.tsc"));
        assert_eq!(c.msr[0].write_param.as_deref(), Some("vclock.tsc.write"));

        assert_eq!(c.leaf_entry_count(0x1), 2);
        assert_eq!(c.leaf_entry_count(0x4), 1);
        assert_eq!(c.leaf_entry_count(0x99), 0);
    }

    fn pcfg(cases: u32) -> ProptestConfig {
        let mut cfg = ProptestConfig::with_cases(if cfg!(miri) { 16 } else { cases });
        if cfg!(miri) {
            cfg.failure_persistence = None;
        }
        cfg
    }

    proptest! {
        #![proptest_config(pcfg(256))]

        #[test]
        fn prop_int_token_roundtrips(n in 0i64..=10_000_000) {
            prop_assert_eq!(parse_value(&n.to_string()), TomlValue::Int(n));
        }

        #[test]
        fn prop_quoted_string_roundtrips(s in "[A-Za-z0-9_.:/ -]{0,24}") {
            prop_assert_eq!(parse_value(&format!("\"{s}\"")), TomlValue::Str(s));
        }

        #[test]
        fn prop_parse_value_never_panics(s in ".{0,40}") {
            let _ = parse_value(&s);
        }
    }
}
