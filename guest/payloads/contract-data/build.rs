//! Generate the conformance tables from `docs/cpu-msr-contract.toml`.
//!
//! The contract is the normative, hashed surface (the `cpuid-model.md` fragment
//! is explicitly non-normative and, post `det-cfl-v1` re-baseline, stale). Its
//! grammar is mechanically canonical (file header §6), so a small line-oriented
//! parser — std only, no TOML crate — extracts exactly what the sweep payloads
//! pin: the frozen CPUID model, the allowed/denied MSR sets and the allow-fixed
//! values, plus the frozen frequency scalars. Emitting these means a contract
//! bump regenerates the tables and surfaces as a payload/golden diff.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

/// One `[...]`-headed block of the TOML and its `key = raw` body lines.
struct Block {
    header: String,
    keys: Vec<(String, String)>,
}

fn split_blocks(text: &str) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    for raw in text.lines() {
        let line = match raw.find('#') {
            // Strip trailing comments, but only outside a quoted string; the
            // contract never quotes a '#', so a simple split is exact here.
            Some(i) if !raw[..i].contains('"') => &raw[..i],
            _ => raw,
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            blocks.push(Block {
                header: line.to_string(),
                keys: Vec::new(),
            });
        } else if let Some(eq) = line.find('=')
            && let Some(b) = blocks.last_mut()
        {
            let key = line[..eq].trim().to_string();
            let val = line[eq + 1..].trim().to_string();
            b.keys.push((key, val));
        }
    }
    blocks
}

fn get<'a>(b: &'a Block, key: &str) -> Option<&'a str> {
    b.keys
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn unquote(s: &str) -> &str {
    s.trim().trim_matches('"')
}

fn hex32(s: &str) -> u32 {
    let s = unquote(s);
    u32::from_str_radix(s.trim_start_matches("0x"), 16)
        .unwrap_or_else(|_| panic!("bad u32 hex: {s:?}"))
}

fn hex64(s: &str) -> u64 {
    let s = unquote(s);
    u64::from_str_radix(s.trim_start_matches("0x"), 16)
        .unwrap_or_else(|_| panic!("bad u64 hex: {s:?}"))
}

/// Parse a subleaf token to a single concrete probe subleaf: `*` -> 0, `Nx+`
/// (and-up) -> N, `a-b` (range) -> a, else the concrete hex.
fn parse_subleaf(s: &str) -> u32 {
    let s = unquote(s);
    if s == "*" {
        0
    } else if let Some(lo) = s.split('+').next().filter(|_| s.contains('+')) {
        hex32(lo)
    } else if let Some(lo) = s.split('-').next().filter(|_| s.contains('-')) {
        hex32(lo)
    } else {
        hex32(s)
    }
}

/// Parse a CPUID register cell to `(base_value, is_dynamic)`. The three `dyn:`
/// forms carry a base the payload reports but does not exact-compare (the live
/// value is a pure function of guest state).
fn parse_reg(s: &str) -> (u32, bool) {
    let s = unquote(s);
    if let Some(base) = s.strip_prefix("dyn:osxsave:") {
        (hex32(base), true)
    } else if let Some(base) = s.strip_prefix("dyn:level-echo:") {
        (hex32(base), true)
    } else if s == "dyn:xcr0-xsavesize" {
        (0, true)
    } else {
        (hex32(s), false)
    }
}

/// Concrete indices a single MSR row contributes (single / range / members).
fn msr_indices(b: &Block) -> Vec<(u32, u32)> {
    if let Some(i) = get(b, "index") {
        vec![(hex32(i), hex32(i))]
    } else if let (Some(lo), Some(hi)) = (get(b, "index-lo"), get(b, "index-hi")) {
        vec![(hex32(lo), hex32(hi))]
    } else if let Some(members) = get(b, "index-members") {
        members
            .trim_matches(['[', ']'].as_ref())
            .split(',')
            .map(|m| m.trim())
            .filter(|m| !m.is_empty())
            .map(|m| {
                let v = hex32(m);
                (v, v)
            })
            .collect()
    } else {
        Vec::new()
    }
}

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").unwrap();
    let contract = Path::new(&manifest).join("../../../docs/cpu-msr-contract.toml");
    let text = fs::read_to_string(&contract)
        .unwrap_or_else(|e| panic!("read {}: {e}", contract.display()));
    println!("cargo:rerun-if-changed={}", contract.display());
    println!("cargo:rerun-if-changed=build.rs");

    let blocks = split_blocks(&text);

    // --- [contract] scalars + [cpuid] header ---
    let contract_blk = blocks
        .iter()
        .find(|b| b.header == "[contract]")
        .expect("[contract]");
    let version: u32 = get(contract_blk, "version")
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let tsc_hz: u64 = get(contract_blk, "tsc-hz").unwrap().trim().parse().unwrap();
    let crystal_hz: u64 = get(contract_blk, "crystal-hz")
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let bus_hz: u64 = get(contract_blk, "bus-hz").unwrap().trim().parse().unwrap();
    let cpuid_blk = blocks
        .iter()
        .find(|b| b.header == "[cpuid]")
        .expect("[cpuid]");
    let max_basic = hex32(get(cpuid_blk, "max-basic-leaf").unwrap());
    let max_extended = hex32(get(cpuid_blk, "max-extended-leaf").unwrap());

    // --- CPUID entries (one concrete probe per [[cpuid.entry]] row) ---
    let mut cpuid = String::new();
    let mut cpuid_count = 0usize;
    for b in blocks.iter().filter(|b| b.header == "[[cpuid.entry]]") {
        let leaf = match (get(b, "leaf"), get(b, "leaf-lo")) {
            (Some(l), _) => hex32(l),
            (None, Some(lo)) => hex32(lo),
            _ => panic!("cpuid.entry without leaf/leaf-lo"),
        };
        let subleaf = parse_subleaf(get(b, "subleaf").expect("subleaf"));
        let (eax, ad) = parse_reg(get(b, "eax").unwrap());
        let (ebx, bd) = parse_reg(get(b, "ebx").unwrap());
        let (ecx, cd) = parse_reg(get(b, "ecx").unwrap());
        let (edx, dd) = parse_reg(get(b, "edx").unwrap());
        let dyn_mask =
            u8::from(ad) | (u8::from(bd) << 1) | (u8::from(cd) << 2) | (u8::from(dd) << 3);
        writeln!(
            cpuid,
            "    CpuidEntry {{ leaf: {leaf:#010x}, subleaf: {subleaf:#010x}, eax: {eax:#010x}, ebx: {ebx:#010x}, ecx: {ecx:#010x}, edx: {edx:#010x}, dyn_mask: {dyn_mask:#04x} }},"
        )
        .unwrap();
        cpuid_count += 1;
    }

    // --- MSR sets ---
    let mut fixed = String::new();
    let mut fixed_count = 0usize;
    let mut stateful: Vec<u32> = Vec::new();
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for b in blocks.iter().filter(|b| b.header == "[[msr.entry]]") {
        let idxs = msr_indices(b);
        ranges.extend(&idxs);
        match get(b, "read").map(unquote) {
            Some("allow-fixed") => {
                // allow-fixed rows are always single-index with a 16-hex param.
                if let Some(i) = get(b, "index") {
                    let v = hex64(get(b, "read-param").expect("allow-fixed read-param"));
                    writeln!(
                        fixed,
                        "    MsrFixed {{ index: {:#06x}, value: {v:#018x} }},",
                        hex32(i)
                    )
                    .unwrap();
                    fixed_count += 1;
                }
            }
            Some("allow-stateful") => {
                for (lo, hi) in &idxs {
                    for i in *lo..=*hi {
                        stateful.push(i);
                    }
                }
            }
            _ => {}
        }
    }

    // Merge the full index set into disjoint sorted ranges for is_contract_msr.
    ranges.sort_unstable();
    let mut merged: Vec<(u32, u32)> = Vec::new();
    for (lo, hi) in ranges {
        match merged.last_mut() {
            Some(last) if lo <= last.1.saturating_add(1) => last.1 = last.1.max(hi),
            _ => merged.push((lo, hi)),
        }
    }

    let mut ranges_s = String::new();
    for (lo, hi) in &merged {
        writeln!(ranges_s, "    ({lo:#010x}, {hi:#010x}),").unwrap();
    }
    let mut stateful_s = String::new();
    for i in &stateful {
        writeln!(stateful_s, "    {i:#06x},").unwrap();
    }

    let out = format!(
        "// @generated from docs/cpu-msr-contract.toml by build.rs — do not edit.\n\
         /// Contract `version` this table was generated from.\n\
         pub const CONTRACT_VERSION: u32 = {version};\n\
         /// Frozen TSC frequency (Hz).\n\
         pub const TSC_HZ: u64 = {tsc_hz};\n\
         /// Frozen core-crystal frequency (Hz).\n\
         pub const CRYSTAL_HZ: u64 = {crystal_hz};\n\
         /// Frozen bus frequency (Hz).\n\
         pub const BUS_HZ: u64 = {bus_hz};\n\
         /// Frozen maximum basic CPUID leaf.\n\
         pub const MAX_BASIC_LEAF: u32 = {max_basic:#010x};\n\
         /// Frozen maximum extended CPUID leaf.\n\
         pub const MAX_EXTENDED_LEAF: u32 = {max_extended:#010x};\n\n\
         /// Frozen CPUID model: one concrete (leaf, subleaf) probe per contract row.\n\
         pub static CPUID_ENTRIES: &[CpuidEntry] = &[\n{cpuid}];\n\
         /// Number of CPUID rows ({cpuid_count}).\n\
         pub const CPUID_COUNT: usize = {cpuid_count};\n\n\
         /// MSRs whose read returns a fixed contract value (allow-fixed).\n\
         pub static MSR_ALLOWED_FIXED: &[MsrFixed] = &[\n{fixed}];\n\
         /// Number of allow-fixed MSRs ({fixed_count}).\n\
         pub const MSR_ALLOWED_FIXED_COUNT: usize = {fixed_count};\n\n\
         /// MSRs whose read/write round-trips guest state (allow-stateful).\n\
         pub static MSR_ALLOWED_STATEFUL: &[u32] = &[\n{stateful_s}];\n\n\
         /// Disjoint sorted ranges covering EVERY MSR index named in the contract,\n\
         /// for [`is_contract_msr`]; anything outside hits the default-deny rule.\n\
         pub static MSR_CONTRACT_RANGES: &[(u32, u32)] = &[\n{ranges_s}];\n"
    );

    let out_dir = env::var("OUT_DIR").unwrap();
    fs::write(Path::new(&out_dir).join("contract_generated.rs"), out).unwrap();
}
