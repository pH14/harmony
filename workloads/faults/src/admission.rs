// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs,
    io::{ErrorKind, Read},
    path::{Path, PathBuf},
};

use iced_x86::{Decoder, DecoderOptions, Mnemonic};
use object::read::elf::ProgramHeader;
use object::{Object, ObjectSection};
use serde::Serialize;
use sha2::{Digest, Sha256};

pub const ALLOWLIST_PATH: &str = "etc/harmony/instruction-allowlist";
pub const ATTESTATION_PATH: &str = "symbols/harmony-instrumented-events";

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct InstructionSite {
    pub digest: String,
    pub address: u64,
    pub instruction: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExecutableScan {
    pub path: String,
    pub digest: String,
    pub sites: Vec<InstructionSite>,
}

#[derive(Clone, Debug, Serialize)]
pub struct InstrumentedExecutable {
    pub path: String,
    pub digest: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ImageReport {
    pub executables: Vec<ExecutableScan>,
    pub instrumented: Vec<InstrumentedExecutable>,
    pub scan_errors: Vec<String>,
    pub attestation_errors: Vec<String>,
    pub runtime: bool,
    pub symbols: bool,
}

impl ImageReport {
    pub fn scan_passed(&self) -> bool {
        self.scan_errors.is_empty()
    }

    pub fn attestation_passed(&self) -> bool {
        self.attestation_errors.is_empty() && !self.instrumented.is_empty()
    }

    pub fn instrumented_events(&self) -> bool {
        self.attestation_passed() && self.runtime && self.symbols
    }

    pub fn passed(&self) -> bool {
        self.scan_passed() && self.instrumented_events()
    }

    pub fn require_admission(&self) -> Result<(), Box<dyn Error>> {
        if !self.scan_passed() || !self.attestation_errors.is_empty() {
            return Err(self
                .scan_errors
                .iter()
                .chain(&self.attestation_errors)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
                .into());
        }
        Ok(())
    }
}

pub fn inspect_image(image: &str) -> Result<ImageReport, Box<dyn Error>> {
    let staging = tempfile::tempdir()?;
    let staged = oci_support::image::stage(image, staging.path())?;
    inspect_root(&staged.rootfs)
}

pub fn inspect_root(root: &Path) -> Result<ImageReport, Box<dyn Error>> {
    let root = root.canonicalize()?;
    let mut files = Vec::new();
    collect_files(&root, &mut files)?;
    files.sort();
    let mut report = ImageReport::default();
    let mut found = BTreeSet::new();
    for path in files {
        let mut file = fs::File::open(&path)?;
        let mut magic = [0; 4];
        match file.read_exact(&mut magic) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::UnexpectedEof => continue,
            Err(error) => return Err(error.into()),
        }
        if &magic != b"\x7fELF" {
            continue;
        }
        let mut bytes = magic.to_vec();
        file.read_to_end(&mut bytes)?;
        let image_path = format!("/{}", path.strip_prefix(&root)?.display());
        let digest = format!("{:x}", Sha256::digest(&bytes));
        match scan_elf(&bytes, &digest) {
            Ok(None) => {}
            Ok(Some(sites)) => {
                found.extend(sites.iter().cloned());
                report.executables.push(ExecutableScan {
                    path: image_path,
                    digest,
                    sites,
                });
            }
            Err(error) => report.scan_errors.push(format!("{image_path}: {error}")),
        }
    }
    let reviewed = match read_allowlist(&root) {
        Ok(reviewed) => reviewed,
        Err(error) => {
            report.scan_errors.push(error.to_string());
            BTreeMap::new()
        }
    };
    for site in &found {
        if !reviewed.contains_key(site) {
            let paths = report
                .executables
                .iter()
                .filter(|file| file.digest == site.digest)
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            report.scan_errors.push(format!(
                "unreviewed instruction: {} 0x{:x} {} ({paths})",
                site.digest, site.address, site.instruction
            ));
        }
    }
    for site in reviewed.keys() {
        if !found.contains(site) {
            report.scan_errors.push(format!(
                "stale reviewed instruction: {} 0x{:x} {}",
                site.digest, site.address, site.instruction
            ));
        }
    }
    read_attestation(&root, &mut report);
    report.runtime = rooted_file(&root, "usr/lib/libvoidstar.so").is_some();
    report.symbols = rooted_file(&root, "symbols/harmony-instrumented-events").is_some()
        && fs::read_dir(root.join("symbols"))
            .into_iter()
            .flatten()
            .flatten()
            .any(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.ends_with(".sym.tsv"))
                    && entry.path().canonicalize().is_ok_and(|path| {
                        path.starts_with(&root)
                            && path.is_file()
                            && fs::metadata(path).is_ok_and(|metadata| metadata.len() > 0)
                    })
            });
    Ok(report)
}

fn collect_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect_files(&entry.path(), files)?;
        } else if kind.is_file() {
            files.push(entry.path());
        }
    }
    Ok(())
}

fn rooted_file(root: &Path, relative: &str) -> Option<PathBuf> {
    let path = root.join(relative).canonicalize().ok()?;
    (path.starts_with(root) && path.is_file()).then_some(path)
}

fn read_allowlist(root: &Path) -> Result<BTreeMap<InstructionSite, String>, Box<dyn Error>> {
    let mut sites = BTreeMap::new();
    let path = root.join(ALLOWLIST_PATH);
    if !path.try_exists()? {
        return Ok(sites);
    }
    let path = rooted_file(root, ALLOWLIST_PATH).ok_or("instruction allowlist leaves the image")?;
    for (index, line) in fs::read_to_string(path)?.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() < 4 || !valid_digest(fields[0]) {
            return Err(format!("invalid instruction allowlist line {}", index + 1).into());
        }
        let address = u64::from_str_radix(
            fields[1]
                .strip_prefix("0x")
                .ok_or("instruction address requires 0x")?,
            16,
        )?;
        let site = InstructionSite {
            digest: fields[0].to_ascii_lowercase(),
            address,
            instruction: fields[2].to_owned(),
        };
        if sites.insert(site, fields[3..].join(" ")).is_some() {
            return Err("duplicate instruction allowlist site".into());
        }
    }
    Ok(sites)
}

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn read_attestation(root: &Path, report: &mut ImageReport) {
    if !root.join(ATTESTATION_PATH).exists() {
        return;
    }
    let Some(path) = rooted_file(root, ATTESTATION_PATH) else {
        report
            .attestation_errors
            .push("instrumentation attestation leaves the image".into());
        return;
    };
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            report
                .attestation_errors
                .push(format!("instrumentation attestation: {error}"));
            return;
        }
    };
    let mut paths = BTreeSet::new();
    for (index, line) in text.lines().enumerate() {
        let result = (|| -> Result<InstrumentedExecutable, Box<dyn Error>> {
            let (expected, path) = line
                .split_once(char::is_whitespace)
                .ok_or("requires a digest and absolute image path")?;
            let path = path.trim();
            if !valid_digest(expected) || !path.starts_with('/') || !paths.insert(path) {
                return Err("invalid digest, non-absolute path, or duplicate executable".into());
            }
            let executable = rooted_file(root, path.trim_start_matches('/'))
                .ok_or("attested executable is missing or leaves the image")?;
            let digest = format!("{:x}", Sha256::digest(fs::read(executable)?));
            if digest != expected.to_ascii_lowercase() {
                return Err(format!("instrumentation digest mismatch: {path}").into());
            }
            Ok(InstrumentedExecutable {
                path: path.to_owned(),
                digest,
            })
        })();
        match result {
            Ok(executable) => report.instrumented.push(executable),
            Err(error) => report.attestation_errors.push(format!(
                "instrumentation attestation line {}: {error}",
                index + 1
            )),
        }
    }
    if paths.is_empty() {
        report
            .attestation_errors
            .push("empty instrumentation attestation".into());
    }
}

fn scan_elf(bytes: &[u8], digest: &str) -> Result<Option<Vec<InstructionSite>>, Box<dyn Error>> {
    let object::File::Elf64(elf) = object::File::parse(bytes)? else {
        return Err("requires ELF64 little-endian".into());
    };
    if !elf.is_little_endian() {
        return Err("requires ELF64 little-endian".into());
    }
    if !matches!(
        elf.kind(),
        object::ObjectKind::Executable | object::ObjectKind::Dynamic
    ) {
        return Ok(None);
    }
    if !matches!(
        elf.architecture(),
        object::Architecture::X86_64 | object::Architecture::Aarch64
    ) {
        return Err("unsupported ELF architecture".into());
    }
    let executable_sections = elf
        .sections()
        .filter(|section| {
            matches!(
                section.flags(),
                object::SectionFlags::Elf { sh_flags }
                    if sh_flags & u64::from(object::elf::SHF_EXECINSTR) != 0
            )
        })
        .map(|section| section.address())
        .collect::<Vec<_>>();
    let writable_executable = object::elf::PF_W | object::elf::PF_X;
    for header in elf.elf_program_headers() {
        let flags = header.p_flags(elf.endian());
        match header.p_type(elf.endian()) {
            object::elf::PT_GNU_STACK if flags & object::elf::PF_X != 0 => {
                return Err("executable ELF stack".into());
            }
            object::elf::PT_LOAD if flags & writable_executable == writable_executable => {
                return Err("writable executable ELF segment".into());
            }
            object::elf::PT_LOAD
                if flags & object::elf::PF_X != 0 && header.p_filesz(elf.endian()) != 0 =>
            {
                let start = header.p_vaddr(elf.endian());
                let end = start.saturating_add(header.p_memsz(elf.endian()));
                if !executable_sections
                    .iter()
                    .any(|address| (start..end).contains(address))
                {
                    return Err("executable ELF segment without an executable section".into());
                }
            }
            _ => {}
        }
    }
    let mut sites = Vec::new();
    for section in elf.sections() {
        let object::SectionFlags::Elf { sh_flags } = section.flags() else {
            continue;
        };
        if sh_flags & u64::from(object::elf::SHF_EXECINSTR) == 0
            || section.kind() == object::SectionKind::UninitializedData
        {
            continue;
        }
        for (address, instruction) in
            scan_code(elf.architecture(), section.address(), section.data()?)?
        {
            sites.push(InstructionSite {
                digest: digest.to_owned(),
                address,
                instruction: instruction.into(),
            });
        }
    }
    sites.sort();
    sites.dedup();
    Ok(Some(sites))
}

fn scan_code(
    architecture: object::Architecture,
    address: u64,
    data: &[u8],
) -> Result<Vec<(u64, &'static str)>, Box<dyn Error>> {
    let mut sites = Vec::new();
    if architecture == object::Architecture::X86_64 {
        let decoder = Decoder::with_ip(64, data, address, DecoderOptions::NONE);
        for instruction in decoder {
            let name = match instruction.mnemonic() {
                Mnemonic::Rdrand => "RDRAND",
                Mnemonic::Rdseed => "RDSEED",
                _ => continue,
            };
            sites.push((instruction.ip(), name));
        }
    } else {
        if !address.is_multiple_of(4) || !data.len().is_multiple_of(4) {
            return Err("unaligned AArch64 executable section".into());
        }
        for (index, bytes) in data.chunks_exact(4).enumerate() {
            let word = u32::from_le_bytes(bytes.try_into()?);
            let name = match word & !0x1f {
                0xd53b2400 => "RNDR",
                0xd53b2420 => "RNDRRS",
                _ => continue,
            };
            sites.push((
                address
                    .checked_add((index as u64) * 4)
                    .ok_or("instruction address overflow")?,
                name,
            ));
        }
    }
    Ok(sites)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elf(code: &[u8], load_flags: u32, stack_flags: u32) -> Vec<u8> {
        let mut bytes = vec![0; 385 + code.len()];
        bytes[..16].copy_from_slice(b"\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00");
        for (offset, value) in [
            (16, 2_u16),
            (18, 62),
            (52, 64),
            (54, 56),
            (56, 2),
            (58, 64),
            (60, 3),
        ] {
            bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        }
        bytes[20..24].copy_from_slice(&1_u32.to_le_bytes());
        bytes[62..64].copy_from_slice(&2_u16.to_le_bytes());
        for (offset, value) in [(24, 0x1000_u64), (32, 64), (40, 192)] {
            bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        bytes[64..68].copy_from_slice(&object::elf::PT_LOAD.to_le_bytes());
        bytes[68..72].copy_from_slice(&load_flags.to_le_bytes());
        for (offset, value) in [
            (72, 384_u64),
            (80, 0x1000),
            (96, code.len() as u64),
            (104, code.len() as u64),
            (112, 1),
        ] {
            bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        bytes[120..124].copy_from_slice(&object::elf::PT_GNU_STACK.to_le_bytes());
        bytes[124..128].copy_from_slice(&stack_flags.to_le_bytes());
        bytes[260..264].copy_from_slice(&object::elf::SHT_PROGBITS.to_le_bytes());
        for (offset, value) in [
            (264, 6_u64),
            (272, 0x1000),
            (280, 384),
            (288, code.len() as u64),
            (304, 1),
        ] {
            bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        bytes[324..328].copy_from_slice(&object::elf::SHT_STRTAB.to_le_bytes());
        bytes[344..352].copy_from_slice(&(384_u64 + code.len() as u64).to_le_bytes());
        bytes[352..360].copy_from_slice(&1_u64.to_le_bytes());
        bytes[368..376].copy_from_slice(&1_u64.to_le_bytes());
        bytes[384..384 + code.len()].copy_from_slice(code);
        bytes
    }

    #[test]
    fn reviewed_instruction_sites_are_bound_to_the_exact_executable_digest() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("etc/harmony")).unwrap();
        let bytes = elf(&[0x0f, 0xc7, 0xf0, 0xc3], 5, 6);
        fs::write(root.path().join("node"), &bytes).unwrap();
        let rejected = inspect_root(root.path()).unwrap();
        assert_eq!(rejected.executables.len(), 1, "{rejected:?}");
        assert_eq!(rejected.scan_errors.len(), 1);
        assert!(rejected.require_admission().is_err());
        fs::write(
            root.path().join(ALLOWLIST_PATH),
            format!(
                "{:x} 0x1000 RDRAND guarded by a CPUID check\n",
                Sha256::digest(&bytes)
            ),
        )
        .unwrap();
        let accepted = inspect_root(root.path()).unwrap();
        assert!(accepted.scan_passed());
        assert!(accepted.require_admission().is_ok());
        fs::write(
            root.path().join("node"),
            elf(&[0x0f, 0xc7, 0xf0, 0xc3, 0x90], 5, 6),
        )
        .unwrap();
        let changed = inspect_root(root.path()).unwrap();
        assert_eq!(changed.scan_errors.len(), 2);
        assert!(changed.require_admission().is_err());
    }

    #[test]
    fn writable_executable_segments_and_executable_stacks_fail() {
        assert!(
            scan_elf(&elf(&[0xc3], 7, 6), "")
                .unwrap_err()
                .to_string()
                .contains("writable executable")
        );
        assert!(
            scan_elf(&elf(&[0xc3], 5, 7), "")
                .unwrap_err()
                .to_string()
                .contains("executable ELF stack")
        );
    }

    #[test]
    fn x86_entropy_sites_follow_instruction_boundaries_and_skip_trapped_counters() {
        let code = [
            0x48, 0x8d, 0x3d, 0x0f, 0xc7, 0xf0, 0, 0x0f, 0x31, 0x0f, 0x01, 0xf9, 0x48, 0x0f, 0xc7,
            0xf0, 0x0f, 0xc7, 0xf8,
        ];
        assert_eq!(
            scan_code(object::Architecture::X86_64, 0x1000, &code).unwrap(),
            [(0x100c, "RDRAND"), (0x1010, "RDSEED")]
        );
    }

    #[test]
    fn arm64_entropy_sites_ignore_the_destination_register_and_skip_trapped_counters() {
        let code: Vec<u8> = [
            0xd53b240f_u32,
            0xd53b243e,
            0xd53be041,
            0xd53be022,
            0xd65f03c0,
        ]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
        assert_eq!(
            scan_code(object::Architecture::Aarch64, 0x400, &code).unwrap(),
            [(0x400, "RNDR"), (0x404, "RNDRRS")]
        );
        assert!(scan_code(object::Architecture::Aarch64, 1, &code).is_err());
    }

    #[test]
    fn executable_segments_without_executable_sections_fail() {
        let mut stripped = elf(&[0x0f, 0xc7, 0xf0], 5, 6);
        stripped[40..48].copy_from_slice(&0_u64.to_le_bytes());
        stripped[60..64].copy_from_slice(&0_u32.to_le_bytes());
        assert!(
            scan_elf(&stripped, "")
                .unwrap_err()
                .to_string()
                .contains("without an executable section")
        );
        let mut data = elf(&[0x0f, 0xc7, 0xf0], 5, 6);
        data[264..272].copy_from_slice(&2_u64.to_le_bytes());
        assert!(scan_elf(&data, "").is_err());
    }

    #[test]
    fn relocatable_objects_and_sections_without_file_bytes_are_not_scanned() {
        let mut relocatable = elf(&[0x0f, 0xc7, 0xf0], 5, 6);
        relocatable[16..18].copy_from_slice(&object::elf::ET_REL.to_le_bytes());
        assert!(scan_elf(&relocatable, "").unwrap().is_none());
        let mut debug = elf(&[0x0f, 0xc7, 0xf0], 5, 6);
        debug[260..264].copy_from_slice(&object::elf::SHT_NOBITS.to_le_bytes());
        assert_eq!(scan_elf(&debug, "").unwrap(), Some(Vec::new()));
    }

    #[test]
    fn a_valid_attestation_line_does_not_hide_a_second_bad_line() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("symbols")).unwrap();
        fs::write(root.path().join("node"), b"node").unwrap();
        fs::write(
            root.path().join(ATTESTATION_PATH),
            format!(
                "{:x}  /node\n{}  /node-missing\n",
                Sha256::digest(b"node"),
                "0".repeat(64)
            ),
        )
        .unwrap();
        let report = inspect_root(root.path()).unwrap();
        assert_eq!(report.instrumented.len(), 1);
        assert_eq!(report.attestation_errors.len(), 1);
        assert!(!report.attestation_passed());
        assert!(report.require_admission().is_err());
    }

    #[test]
    fn malformed_elf_and_stale_reviews_fail_admission() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("etc/harmony")).unwrap();
        fs::write(root.path().join("broken"), b"\x7fELFbroken").unwrap();
        fs::write(
            root.path().join(ALLOWLIST_PATH),
            format!("{} 0x4 RDRAND guarded-by-cpuid\n", "0".repeat(64)),
        )
        .unwrap();
        let report = inspect_root(root.path()).unwrap();
        assert_eq!(report.scan_errors.len(), 2);
        assert!(report.require_admission().is_err());
    }
}
