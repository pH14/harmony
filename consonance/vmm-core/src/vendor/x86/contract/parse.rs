// SPDX-License-Identifier: AGPL-3.0-or-later
//! A tiny, total TOML-subset reader for `docs/cpu-msr-contract.toml` and the
//! typed [`Contract`] it produces.
//!
//! The contract artifact is **trusted, compile-time-embedded** data (`include_str!`
//! in the parent module), not untrusted guest input — so this parser may use
//! commented `expect`s on the known-good grammar (conventions rule-4's no-panic
//! rule governs untrusted input; a malformed *embedded* contract is a build bug
//! caught by the `#[cfg(test)]` validation below). The grammar is the strict,
//! mechanically-canonical subset the TOML's own header documents: `[section]` and
//! `[[array.entry]]` headers, and `key = "string" | int | true/false | ["a","b"]`
//! values, one per line, with `#` line/inline comments.
//!
//! `vmm-core` owns the canonical serialization (`super::canonical`): the same
//! parsed tables feed both the runtime policy (`super`) and the §6 `contract_hash`,
//! so what is hashed is what is enforced (CPU-MSR-CONTRACT §6), with no second
//! hand-maintained copy.

use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Vendor axis (Deliverable 1 — a vendor column on the one frozen contract).
// ---------------------------------------------------------------------------

/// The x86 vendor a contract file is a column for. Both Intel and AMD are the
/// **same `Arch`** (x86-64, `docs/ARCH-BOUNDARY.md`); the vendor is a first-class
/// axis *inside* `vendor/x86/contract/`, not a second `Arch`. The `GenuineIntel`
/// column (`docs/cpu-msr-contract.toml`, `det-cfl-v1`) is current truth; the
/// `AuthenticAMD` column (`docs/cpu-msr-contract-amd-draft.toml`, `det-zenN-v1`)
/// is a draft, `verify-on-silicon` pending AE-4.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VendorId {
    /// Intel — the ratified, live-enforced column.
    GenuineIntel,
    /// AMD — the draft column, wired into no live enforcement path.
    AuthenticAMD,
}

impl VendorId {
    /// The 12-byte CPUID leaf-0 vendor string (EBX‖EDX‖ECX) this vendor freezes.
    pub(crate) const fn cpuid_string(self) -> &'static str {
        match self {
            VendorId::GenuineIntel => "GenuineIntel",
            VendorId::AuthenticAMD => "AuthenticAMD",
        }
    }

    /// The `[contract] vendor` header token.
    fn as_token(self) -> &'static str {
        match self {
            VendorId::GenuineIntel => "GenuineIntel",
            VendorId::AuthenticAMD => "AuthenticAMD",
        }
    }

    /// Parse a `[contract] vendor` token; `None` for any unrecognized string.
    fn from_token(s: &str) -> Option<VendorId> {
        match s {
            "GenuineIntel" => Some(VendorId::GenuineIntel),
            "AuthenticAMD" => Some(VendorId::AuthenticAMD),
            _ => None,
        }
    }
}

/// A refusal from the vendor-axis loader ([`Contract::load`]). Trusted embedded
/// contract data never trips these at runtime; they exist so a *mismatched* axis
/// (loading the AMD draft under the Intel axis, or an artifact whose declared
/// vendor disagrees with its own CPUID leaf-0 string) is a loud, testable refusal
/// rather than a silently-wrong policy.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum ContractError {
    /// The `[contract] vendor` header disagrees with the axis the file was loaded
    /// under (e.g. the AuthenticAMD draft loaded under the GenuineIntel axis).
    #[error("contract vendor mismatch: file declares {found}, loaded under {expected}")]
    VendorMismatch {
        expected: &'static str,
        found: String,
    },
    /// The declared vendor disagrees with the file's own CPUID leaf-0 vendor
    /// string — a mixed-vendor artifact (Deliverable 8's structural guard).
    #[error("mixed-vendor artifact: declares vendor {declared}, but CPUID leaf 0 spells {leaf0}")]
    MixedVendor {
        declared: &'static str,
        leaf0: String,
    },
    /// The `[contract] vendor` header is present but not a recognized vendor token.
    /// Fail-closed: a present-but-invalid axis is **refused**, never silently
    /// defaulted to GenuineIntel (only a genuinely *absent* key defaults).
    #[error("unknown contract vendor token {token:?} (expected GenuineIntel or AuthenticAMD)")]
    UnknownVendor { token: String },
    /// CPUID leaf 0 is **present** but is not a readable frozen vendor string — its
    /// registers use a dynamic rule, or its constant bytes are not UTF-8. The
    /// mixed-vendor guard cannot be bypassed by a malformed leaf 0; only a genuinely
    /// *absent* leaf 0 is exempt.
    #[error(
        "malformed CPUID leaf 0 under vendor {declared}: not three frozen UTF-8 vendor-string constants"
    )]
    MalformedLeaf0 { declared: &'static str },
}

/// A parsed TOML scalar/array value (the only shapes this contract uses).
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

/// The raw table layout: singleton `[section]`s and arrays of `[[a.entry]]`s.
struct Raw {
    singletons: BTreeMap<String, BTreeMap<String, TomlValue>>,
    arrays: BTreeMap<String, Vec<BTreeMap<String, TomlValue>>>,
}

/// Strip a `#` line/inline comment, honoring `"`-quoted spans.
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

/// Parse a single value token (`"str"`, int, bool, or `["a", "b"]`).
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
        // Decimal integer (the only bare-number values in the contract).
        TomlValue::Int(s.parse().unwrap_or(0))
    }
}

/// The current insertion target while scanning lines.
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

// ---------------------------------------------------------------------------
// Typed rows.
// ---------------------------------------------------------------------------

/// A CPUID leaf token: a single leaf or an inclusive `lo-hi` range.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LeafSpec {
    pub lo: u32,
    pub hi: u32,
}

/// A CPUID subleaf token.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Subleaf {
    Single(u32),
    All,
    AndUp(u32),
    Range(u32, u32),
}

/// A CPUID register field: a frozen constant or one of the three dynamic rules.
#[derive(Clone, Copy, Debug)]
pub(crate) enum RegField {
    Const(u32),
    DynOsxsave(u32),
    DynLevelEcho(u32),
    DynXcr0Xsavesize,
}

impl RegField {
    /// The frozen base value installed into KVM's table (the dynamic cells are
    /// recomputed in-kernel from guest state, so the base is what we hand over).
    pub(crate) fn base(self) -> u32 {
        match self {
            RegField::Const(v) | RegField::DynOsxsave(v) => v,
            // Level-echo base: `type << 8` with input subleaf 0.
            RegField::DynLevelEcho(t) => t << 8,
            // XSAVE-area size for the model's enabled XCR0 (0x7 → 0x340).
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
    /// The `verify-on-silicon` qualifier (Deliverable 3). `None` for Intel rows
    /// (implicitly `verified = det-cfl-v1`, the frozen baseline); `Some(
    /// "on-silicon-pending-AE4")` for every AMD enforcement row. Part of the
    /// hashed canonical form, so a row silently losing its marker is hash-breaking.
    pub verified: Option<String>,
}

/// The set of MSR indices a row names.
#[derive(Clone, Debug)]
pub(crate) enum IndexSpec {
    Single(u32),
    Range(u32, u32),
    Members(Vec<u32>),
}

impl IndexSpec {
    /// Expand to the concrete ascending index list.
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
    /// The `verify-on-silicon` qualifier (Deliverable 3) — see [`CpuidRow::verified`].
    pub verified: Option<String>,
    /// The per-generation PMU marker (Deliverable 4): `Some("legacy-perfmon")` for
    /// the `PERF_CTL`/`PERF_CTR` core pairs, `Some("zen4+")` for the PerfMonV2
    /// global control/status MSRs. The loader parses both and resolves neither —
    /// which set is live for a given part is an AE-0 decision, not an AMD constant.
    /// `None` for every non-PMU row. Part of the hashed canonical form.
    pub applies_when: Option<String>,
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
    /// `where`: `port:0xNN`, `idx:0xNN`, or the `idx:0xLO-0xHI` range form.
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
pub(crate) struct HostAssert {
    pub family_model_stepping: String,
    pub host_microcode_rev: String,
    pub guest_ucode_rev: String,
    pub mxcsr_mask: String,
    pub maxphyaddr_min: i64,
    pub rtm_disabled: bool,
    pub cr4_force_reserved: Vec<String>,
    pub host_absent: Vec<String>,
}

/// The fully-typed contract: every normative table the §6 canonical form covers.
#[derive(Clone, Debug)]
pub(crate) struct Contract {
    /// The vendor axis this file is a column for (Deliverable 1). Parsed from the
    /// `[contract] vendor` header (default [`VendorId::GenuineIntel`] when absent,
    /// for the Intel-flavoured synthetic test fixtures). **Not** emitted into the
    /// hashed canonical form — the zero-drift grammar (Deliverable 6): adding
    /// `vendor` to the Intel header leaves its canonical bytes and `contract_hash`
    /// byte-identical. The axis is enforced at load time by [`Contract::load`], not
    /// carried in the hash.
    pub vendor: VendorId,
    /// The literal `[contract] vendor` token exactly as written (`None` if the key
    /// is absent). `load` reads this to distinguish an **absent** vendor (legacy
    /// Intel fixtures — allowed) from a **present-but-invalid** one (refused), so a
    /// bad token can never silently default to GenuineIntel.
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
    /// The §6 registry hash, if/once the foreman has committed
    /// `contract_hash = "<hex>"` to the `[contract]` table. `None` until then.
    /// **Not** part of the canonical form (it is the hash *of* the body, so it
    /// cannot be in the body) — the serializer never reads it. Read only by the
    /// `#[ignore]`d registry-drift test until the field lands, so the un-ignore is
    /// a one-line change; allow dead_code until then.
    #[allow(dead_code)]
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
    pub host_assert: HostAssert,
    /// Section-level `transfers-unchanged-pending-AE4` markers (Deliverable 2,
    /// veto point 5): the shared-ISA surface the AMD draft carries **by marker**
    /// rather than by hand-copying 3000 near-duplicate rows (never fork the one
    /// reproducer). Keyed by section name (`cpuid-standard`, `msr-shared`, `insn`,
    /// `timer`, `cmos`, `mmio`, `host-assert`) → the transfer disposition
    /// (`unchanged-pending-AE4`, or `on-silicon-pending-AE4` for the per-silicon
    /// host-assert block). The canonicalizer records each marker in place of the
    /// section's rows; empty for the Intel column, which materializes every row.
    pub transfers: BTreeMap<String, String>,
}

/// Parse `"0x...."`/decimal text into a `u32` (trusted contract token).
fn hex32(s: &str) -> u32 {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x") {
        u32::from_str_radix(h, 16).expect("contract: malformed 32-bit hex")
    } else {
        s.parse().expect("contract: malformed 32-bit decimal")
    }
}

/// Parse a CPUID register field token.
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

/// Parse a CPUID subleaf token (`0xNN`, `*`, `0xNN+`, `0xLO-0xHI`).
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

/// Read an optional string-valued key from a row's key map (`None` when absent or
/// empty). Used for the AMD `verified` / `applies-when` qualifiers, which Intel
/// rows omit.
fn opt_str(e: &BTreeMap<String, TomlValue>, key: &str) -> Option<String> {
    e.get(key)
        .map(|v| v.as_str().to_string())
        .filter(|s| !s.is_empty())
}

/// Pull the read/write disposition tokens + optional formula params from a row's
/// key map.
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

/// The 12-char vendor string frozen at a CPUID leaf-0 `row` (EBX‖EDX‖ECX
/// little-endian), or `None` if the row uses dynamic register rules or its constant
/// bytes are not UTF-8 — a **malformed** frozen vendor string, which [`Contract::load`]
/// refuses rather than silently treating as an absent leaf 0.
fn leaf0_vendor_string(row: &CpuidRow) -> Option<String> {
    let (ebx, edx, ecx) = match (row.ebx, row.edx, row.ecx) {
        (RegField::Const(b), RegField::Const(d), RegField::Const(c)) => (b, d, c),
        _ => return None,
    };
    let mut bytes = Vec::with_capacity(12);
    for reg in [ebx, edx, ecx] {
        bytes.extend_from_slice(&reg.to_le_bytes());
    }
    String::from_utf8(bytes).ok()
}

/// Whether a CPUID `row`'s (leaf, subleaf) coverage includes **leaf 0, subleaf 0** —
/// where the vendor string is frozen. This includes the grammar's inclusive
/// **range** form (`leaf-lo = 0, leaf-hi > 0`) and the `*` / `N+` / `a-b` subleaf
/// tokens, not just the single `leaf = 0, subleaf = 0` row: any such row installs a
/// value at CPUID(0,0), so the mixed-vendor guard must inspect every one of them (a
/// range row must not be able to smuggle a foreign vendor past the `lo == hi == 0`
/// check). `leaf.lo` is `u32`, so `lo <= 0` ⟺ `lo == 0`, and then `0 <= hi` always.
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
    /// Parse the embedded contract TOML into typed tables.
    pub(crate) fn parse(toml: &str) -> Contract {
        let raw = parse_raw(toml);
        let empty = BTreeMap::new();
        let c = raw.singletons.get("contract").unwrap_or(&empty);
        let mmio = raw.singletons.get("mmio").unwrap_or(&empty);
        let ha = raw.singletons.get("host-assert").unwrap_or(&empty);

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

        // The vendor axis (Deliverable 1). Keep the raw declared token so `load` can
        // distinguish absent (legacy fixtures — allowed) from present-but-invalid
        // (fail-closed refusal). The resolved `vendor` defaults to GenuineIntel for an
        // absent OR invalid token; `load` refuses an invalid token before it is trusted.
        let vendor_declared = c.get("vendor").map(|v| v.as_str().to_string());
        let vendor = vendor_declared
            .as_deref()
            .and_then(VendorId::from_token)
            .unwrap_or(VendorId::GenuineIntel);

        // Section-level transfer markers (Deliverable 2). The `[transfers]` singleton
        // maps a section name to its transfer disposition.
        let transfers = raw
            .singletons
            .get("transfers")
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().to_string()))
                    .collect()
            })
            .unwrap_or_default();

        Contract {
            vendor,
            vendor_declared,
            transfers,
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
            host_assert: HostAssert {
                family_model_stepping: ha
                    .get("family-model-stepping")
                    .map(|v| v.as_str().to_string())
                    .unwrap_or_default(),
                host_microcode_rev: ha
                    .get("host-microcode-rev")
                    .map(|v| v.as_str().to_string())
                    .unwrap_or_default(),
                guest_ucode_rev: ha
                    .get("guest-ucode-rev")
                    .map(|v| v.as_str().to_string())
                    .unwrap_or_default(),
                mxcsr_mask: ha
                    .get("mxcsr-mask")
                    .map(|v| v.as_str().to_string())
                    .unwrap_or_default(),
                maxphyaddr_min: ha
                    .get("maxphyaddr-min")
                    .map(TomlValue::as_int)
                    .unwrap_or_default(),
                rtm_disabled: ha
                    .get("rtm-disabled")
                    .map(TomlValue::as_bool)
                    .unwrap_or_default(),
                cr4_force_reserved: ha
                    .get("cr4-force-reserved")
                    .map(|v| v.as_arr().to_vec())
                    .unwrap_or_default(),
                host_absent: ha
                    .get("host-absent")
                    .map(|v| v.as_arr().to_vec())
                    .unwrap_or_default(),
            },
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
            verified: opt_str(e, "verified"),
        }
    }

    fn msr_row(e: &BTreeMap<String, TomlValue>) -> MsrRow {
        let index = if let Some(members) = e.get("index-members") {
            IndexSpec::Members(members.as_arr().iter().map(|s| hex32(s)).collect())
        } else if let Some(lo) = e.get("index-lo") {
            IndexSpec::Range(hex32(lo.as_str()), hex32(e["index-hi"].as_str()))
        } else {
            IndexSpec::Single(hex32(e["index"].as_str()))
        };
        let (read, read_param, write, write_param) = dispositions(e);
        MsrRow {
            index,
            read,
            read_param,
            write,
            write_param,
            verified: opt_str(e, "verified"),
            applies_when: opt_str(e, "applies-when"),
        }
    }

    /// Parse + validate the vendor axis (Deliverable 1), **fail-closed**. Returns the
    /// typed contract only if the file's `[contract] vendor` header agrees with
    /// `expected` **and** the file is not a mixed-vendor artifact. Every ambiguity is
    /// a refusal, never a silent default:
    /// - vendor header **absent** → allowed (legacy Intel fixtures);
    /// - vendor header **present but not a known token** → [`ContractError::UnknownVendor`];
    /// - vendor header present, valid, but disagreeing with `expected` →
    ///   [`ContractError::VendorMismatch`];
    /// - CPUID leaf 0 **absent** → the mixed-vendor guard is skipped (fixtures);
    /// - CPUID leaf 0 **present but malformed** (dynamic registers / non-UTF-8 bytes)
    ///   → [`ContractError::MalformedLeaf0`] (the guard cannot be bypassed);
    /// - CPUID leaf 0 present, readable, but spelling another vendor →
    ///   [`ContractError::MixedVendor`].
    ///
    /// The single entry point the vendor-parameterized constructors go through; the
    /// underlying [`Contract::parse`] stays infallible for the direct-token unit tests.
    pub(crate) fn load(toml: &str, expected: VendorId) -> Result<Contract, ContractError> {
        let c = Self::parse(toml);
        // A **present-but-invalid** vendor token is refused, never defaulted
        // (fail-closed); a genuinely absent header resolves `c.vendor` to GenuineIntel.
        if let Some(tok) = c.vendor_declared.as_deref()
            && VendorId::from_token(tok).is_none()
        {
            return Err(ContractError::UnknownVendor {
                token: tok.to_string(),
            });
        }
        // Axis check on the **resolved** vendor (a valid declared token, or the
        // absent-default GenuineIntel — so an absent header loads only under Intel).
        if c.vendor != expected {
            return Err(ContractError::VendorMismatch {
                expected: expected.as_token(),
                found: c.vendor.as_token().to_string(),
            });
        }
        // Mixed-vendor guard: **every** CPUID row that covers leaf 0 subleaf 0 —
        // including the inclusive range form (`leaf-lo = 0, leaf-hi > 0`), which the
        // old `lo == hi == 0` check missed — must freeze a readable vendor string that
        // spells the declared vendor. A present-but-malformed covering row is refused
        // (it may not masquerade as an absent leaf 0); only a genuinely absent leaf 0
        // (no covering row at all) is exempt (the synthetic fixtures omit it).
        for row in c.cpuid.iter().filter(|r| covers_leaf0_subleaf0(r)) {
            let leaf0 = leaf0_vendor_string(row).ok_or(ContractError::MalformedLeaf0 {
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

    /// Per-leaf entry count (used to decide `SIGNIFICANT_INDEX` when building the
    /// KVM model — not part of the hash).
    pub(crate) fn leaf_entry_count(&self, leaf_lo: u32) -> usize {
        self.cpuid.iter().filter(|r| r.leaf.lo == leaf_lo).count()
    }
}

#[cfg(test)]
mod tests {
    //! The contract parser is the **determinism anchor**: it builds the typed
    //! tables the §6 canonical serializer hashes into `contract_hash`. These tests
    //! pin every token form and reject path of the TOML-subset reader and the
    //! bit-packing of the typed rows, so a silent parse regression (the class of bug
    //! that let the `cr4-force-reserved` spelling slip) cannot survive.

    use proptest::prelude::*;

    use super::*;

    // --- TomlValue accessors (incl. the type-mismatch fallback arms) ----------

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
        // Type-mismatch fallbacks: an accessor on the wrong variant returns the
        // documented default (never a panic), so a malformed cell degrades safely.
        assert_eq!(TomlValue::Int(1).as_str(), "");
        assert_eq!(TomlValue::Str("x".into()).as_int(), 0);
        assert!(!TomlValue::Int(1).as_bool());
        // `as_bool` is true ONLY for `Bool(true)` — a `Str("true")` is not a bool.
        assert!(!TomlValue::Str("true".into()).as_bool());
        assert_eq!(TomlValue::Int(1).as_arr(), &[] as &[String]);
    }

    // --- strip_comment --------------------------------------------------------

    #[test]
    fn strip_comment_respects_quoted_hashes() {
        assert_eq!(strip_comment("key = 1 # trailing"), "key = 1 ");
        assert_eq!(strip_comment("# whole line"), "");
        assert_eq!(strip_comment("no comment here"), "no comment here");
        // A '#' inside a quoted string is NOT a comment start.
        assert_eq!(strip_comment("k = \"a#b\""), "k = \"a#b\"");
        // …but a '#' after the closing quote IS.
        assert_eq!(strip_comment("k = \"v\" # c"), "k = \"v\" ");
    }

    // --- parse_value (every token form + reject/empty paths) ------------------

    #[test]
    fn parse_value_classifies_every_token() {
        assert_eq!(parse_value("true"), TomlValue::Bool(true));
        assert_eq!(parse_value("false"), TomlValue::Bool(false));
        assert_eq!(parse_value("  true  "), TomlValue::Bool(true)); // trims first
        assert_eq!(parse_value("46"), TomlValue::Int(46));
        assert_eq!(parse_value("0"), TomlValue::Int(0));
        // A non-numeric bare token degrades to Int(0) (unwrap_or), never panics.
        assert_eq!(parse_value("not_a_number"), TomlValue::Int(0));
        assert_eq!(parse_value("\"hello\""), TomlValue::Str("hello".into()));
        assert_eq!(parse_value("\"\""), TomlValue::Str(String::new()));
        assert_eq!(
            parse_value("[\"a\", \"b\"]"),
            TomlValue::Arr(vec!["a".into(), "b".into()])
        );
        assert_eq!(parse_value("[]"), TomlValue::Arr(vec![]));
        // Empty elements (and a trailing comma) are filtered out — so a stray
        // separator never injects a phantom "" into a hashed array row.
        assert_eq!(
            parse_value("[\"a\", \"\", \"b\", ]"),
            TomlValue::Arr(vec!["a".into(), "b".into()])
        );
    }

    // --- parse_raw (sections, arrays, and the no-target drop) -----------------

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
        // A key before any [section] header (Target::None) is dropped.
        assert!(!raw.singletons.values().any(|m| m.contains_key("stray")));
        assert_eq!(
            raw.singletons.get("contract").unwrap().get("version"),
            Some(&TomlValue::Int(2))
        );
        // Two [[cpuid.entry]] blocks accumulate into the array.
        assert_eq!(raw.arrays.get("cpuid.entry").unwrap().len(), 2);
    }

    // --- hex32 / reg_field / subleaf ------------------------------------------

    #[test]
    fn hex32_parses_hex_and_decimal() {
        assert_eq!(hex32("0x100000"), 0x10_0000);
        assert_eq!(hex32("0x0"), 0);
        assert_eq!(hex32("46"), 46);
        assert_eq!(hex32("  0x1b  "), 0x1b); // trims
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

    // --- RegField::base bit-packing (kills <<→>> and return-0 mutants) ---------

    #[test]
    fn reg_field_base_is_exact() {
        assert_eq!(RegField::Const(0xDEAD_BEEF).base(), 0xDEAD_BEEF);
        assert_eq!(RegField::DynOsxsave(0x76da_3203).base(), 0x76da_3203);
        // level-echo base = `type << 8` — NOT `>> 8`, NOT 0.
        assert_eq!(RegField::DynLevelEcho(0x01).base(), 0x0100);
        assert_eq!(RegField::DynLevelEcho(0x12).base(), 0x1200);
        // XSAVE-area size for the frozen XCR0 (0x7) is the fixed 0x340.
        assert_eq!(RegField::DynXcr0Xsavesize.base(), 0x340);
    }

    // --- IndexSpec::indices (single / range / sorted members) -----------------

    #[test]
    fn index_spec_indices_expands_and_sorts() {
        assert_eq!(IndexSpec::Single(0x10).indices(), vec![0x10]);
        assert_eq!(
            IndexSpec::Range(0x800, 0x803).indices(),
            vec![0x800, 0x801, 0x802, 0x803]
        );
        // Members are returned in ascending order regardless of input order.
        assert_eq!(
            IndexSpec::Members(vec![0x30, 0x10, 0x20]).indices(),
            vec![0x10, 0x20, 0x30]
        );
    }

    // --- dispositions ---------------------------------------------------------

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

        // Absent read/write default to the empty token; absent params to None.
        let (r2, rp2, w2, wp2) = dispositions(&BTreeMap::new());
        assert_eq!((r2.as_str(), rp2, w2.as_str(), wp2), ("", None, "", None));
    }

    // --- cpuid_row / msr_row (every leaf and index form) ----------------------

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

    // --- Full synthetic parse + leaf_entry_count (exact counts) ---------------

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
[host-assert]\n\
maxphyaddr-min = 46\n\
rtm-disabled = true\n\
cr4-force-reserved = [\"PKE\", \"PKS\"]\n";

    #[test]
    fn parse_full_synthetic_contract() {
        let c = Contract::parse(SYNTH);
        assert_eq!(c.version, 2);
        assert_eq!(c.kernel_tag, "v6.18.35");
        assert_eq!(c.cpuid_baseline, "test-baseline");
        assert_eq!(c.tsc_hz, 2_000_000_000);
        assert_eq!(c.mxcsr_mask, "0x0000ffff");
        assert_eq!(c.host_assert.maxphyaddr_min, 46);
        assert!(c.host_assert.rtm_disabled);
        assert_eq!(
            c.host_assert.cr4_force_reserved,
            vec!["PKE".to_string(), "PKS".to_string()]
        );

        assert_eq!(c.cpuid.len(), 3);
        assert!(matches!(c.cpuid[0].ecx, RegField::DynOsxsave(0x76da_3203)));
        assert_eq!(c.msr.len(), 1);
        assert_eq!(c.msr[0].read, "emulate-vtime");
        assert_eq!(c.msr[0].read_param.as_deref(), Some("vclock.tsc"));
        assert_eq!(c.msr[0].write_param.as_deref(), Some("vclock.tsc.write"));

        // leaf_entry_count: EXACT per-leaf counts (kills the `==→!=` filter and the
        // return-value mutants — `!=` would count the *other* leaves, `0` would
        // count none).
        assert_eq!(c.leaf_entry_count(0x1), 2);
        assert_eq!(c.leaf_entry_count(0x4), 1);
        assert_eq!(c.leaf_entry_count(0x99), 0);
    }

    // --- Property: parse is total + classification is stable ------------------

    /// Proptest config that is Miri-safe: fewer cases, and **no failure
    /// persistence** — proptest's regression file resolves a relative path via
    /// `getcwd`, which Miri's isolation blocks (matches `tests/loader_proptest.rs`).
    fn pcfg(cases: u32) -> ProptestConfig {
        let mut cfg = ProptestConfig::with_cases(if cfg!(miri) { 16 } else { cases });
        if cfg!(miri) {
            cfg.failure_persistence = None;
        }
        cfg
    }

    proptest! {
        #![proptest_config(pcfg(256))]

        /// Any decimal integer string parses back to the same `Int` (kills the
        /// arithmetic/`==` mutants in the scalar path under fuzzing too).
        #[test]
        fn prop_int_token_roundtrips(n in 0i64..=10_000_000) {
            prop_assert_eq!(parse_value(&n.to_string()), TomlValue::Int(n));
        }

        /// Any quoted simple string round-trips through the string branch.
        #[test]
        fn prop_quoted_string_roundtrips(s in "[A-Za-z0-9_.:/ -]{0,24}") {
            // The inner text has no embedded quotes, so trim_matches('"') recovers it.
            prop_assert_eq!(parse_value(&format!("\"{s}\"")), TomlValue::Str(s));
        }

        /// `parse_value` is total on arbitrary bytes — every input yields *some*
        /// variant, never a panic (the contract is trusted, but robustness is free).
        #[test]
        fn prop_parse_value_never_panics(s in ".{0,40}") {
            let _ = parse_value(&s);
        }
    }
}
