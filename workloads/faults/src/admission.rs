// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
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

use oci_support::bundle::ArchivedRootfs;

pub const ALLOWLIST_PATH: &str = "etc/harmony/instruction-allowlist";
pub const ATTESTATION_PATH: &str = "symbols/harmony-instrumented-events";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionMode {
    Vm,
    Uml,
}

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

pub struct ArchivedImage<'a> {
    entries: BTreeMap<&'a str, (u32, &'a [u8])>,
}

impl<'a> ArchivedImage<'a> {
    pub fn new(archive: &'a ArchivedRootfs) -> Self {
        Self {
            entries: archive
                .entries()
                .map(|entry| (entry.path, (entry.mode, entry.data)))
                .collect(),
        }
    }

    pub fn file(&self, relative: &str) -> Option<&'a [u8]> {
        let path = self.resolve_path(relative)?;
        let (mode, data) = self.entries.get(path.as_str())?;
        (mode & 0o170000 == 0o100000).then_some(*data)
    }

    fn resolve_path(&self, relative: &str) -> Option<String> {
        let mut pending = relative
            .split('/')
            .map(str::to_owned)
            .collect::<VecDeque<_>>();
        let mut parts = Vec::new();
        let mut links = 0;
        while let Some(component) = pending.pop_front() {
            match component.as_str() {
                "" | "." => continue,
                ".." => {
                    parts.pop()?;
                    continue;
                }
                _ => parts.push(component),
            }
            let path = parts.join("/");
            if let Some((mode, data)) = self.entries.get(path.as_str())
                && mode & 0o170000 == 0o120000
            {
                links += 1;
                if links > 40 {
                    return None;
                }
                let target = std::str::from_utf8(data).ok()?;
                if target.starts_with('/') {
                    return None;
                }
                parts.pop();
                for part in target.split('/').rev() {
                    pending.push_front(part.to_owned());
                }
            }
        }
        Some(parts.join("/"))
    }

    fn mentions(&self, relative: &str) -> bool {
        if let Some(path) = self.resolve_path(relative) {
            return self.entries.contains_key(path.as_str());
        }
        let mut prefix = String::new();
        for component in relative.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            if self
                .entries
                .get(prefix.as_str())
                .is_some_and(|(mode, _)| mode & 0o170000 == 0o120000)
            {
                return true;
            }
        }
        self.entries.contains_key(relative)
    }

    pub fn inspect(&self) -> Result<ImageReport, Box<dyn Error>> {
        let mut report = ImageReport::default();
        let mut found = BTreeSet::new();
        for (&path, &(mode, data)) in &self.entries {
            if mode & 0o170000 == 0o100000 && data.starts_with(b"\x7fELF") {
                scan_file(&mut report, &mut found, &format!("/{path}"), data.to_vec());
            }
        }
        let reviewed = if self.mentions(ALLOWLIST_PATH) {
            let read = self
                .file(ALLOWLIST_PATH)
                .ok_or("instruction allowlist leaves the image")
                .and_then(|bytes| std::str::from_utf8(bytes).map_err(|_| "invalid allowlist UTF-8"))
                .map_err(|error| -> Box<dyn Error> { error.into() })
                .and_then(parse_allowlist);
            match read {
                Ok(reviewed) => reviewed,
                Err(error) => {
                    report.scan_errors.push(error.to_string());
                    BTreeMap::new()
                }
            }
        } else {
            BTreeMap::new()
        };
        finish_scan(&mut report, &found, &reviewed);
        if self.mentions(ATTESTATION_PATH) {
            match self.file(ATTESTATION_PATH) {
                Some(bytes) => match std::str::from_utf8(bytes) {
                    Ok(text) => parse_attestation(text, &mut report, |path| {
                        self.file(path.trim_start_matches('/')).ok_or_else(|| {
                            "attested executable is missing or leaves the image".into()
                        })
                    }),
                    Err(error) => report
                        .attestation_errors
                        .push(format!("instrumentation attestation: {error}")),
                },
                None => report
                    .attestation_errors
                    .push("instrumentation attestation leaves the image".into()),
            }
        }
        report.runtime = self.file("usr/lib/libvoidstar.so").is_some();
        report.symbols = self.file(ATTESTATION_PATH).is_some()
            && self.resolve_path("symbols").is_some_and(|directory| {
                let prefix = format!("{directory}/");
                self.entries.keys().any(|path| {
                    path.strip_prefix(&prefix)
                        .is_some_and(|name| !name.contains('/') && name.ends_with(".sym.tsv"))
                        && self.file(path).is_some_and(|bytes| !bytes.is_empty())
                })
            });
        Ok(report)
    }
}

impl ImageReport {
    pub fn require_for(&self, mode: ExecutionMode) -> Result<(), Box<dyn Error>> {
        match mode {
            ExecutionMode::Vm => self.require_admission(),
            ExecutionMode::Uml => self.require_uml_admission(),
        }
    }

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

    pub fn require_uml_admission(&self) -> Result<(), Box<dyn Error>> {
        self.require_admission()?;
        let native_host_inputs = self
            .executables
            .iter()
            .flat_map(|executable| {
                executable
                    .sites
                    .iter()
                    .filter(|site| {
                        matches!(
                            site.instruction.as_str(),
                            "RDRAND" | "RDSEED" | "RNDR" | "RNDRRS" | "RDPID" | "LSL"
                        )
                    })
                    .map(|site| {
                        format!(
                            "{} 0x{:x} {} ({})",
                            site.digest, site.address, site.instruction, executable.path
                        )
                    })
            })
            .collect::<Vec<_>>();
        if native_host_inputs.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "User-mode Linux executes hardware entropy and CPU-identity instructions natively; these image sites cannot be admitted even when reviewed:\n{}",
                native_host_inputs.join("\n")
            )
            .into())
        }
    }
}

pub fn inspect_image(image: &str) -> Result<ImageReport, Box<dyn Error>> {
    let staging = tempfile::tempdir()?;
    let staged = oci_support::image::stage(image, staging.path())?;
    oci_support::image::with_staged_read_access(&staged, || inspect_root(&staged.rootfs))
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
        scan_file(&mut report, &mut found, &image_path, bytes);
    }
    let reviewed = match read_allowlist(&root) {
        Ok(reviewed) => reviewed,
        Err(error) => {
            report.scan_errors.push(error.to_string());
            BTreeMap::new()
        }
    };
    finish_scan(&mut report, &found, &reviewed);
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

fn scan_file(
    report: &mut ImageReport,
    found: &mut BTreeSet<InstructionSite>,
    image_path: &str,
    bytes: Vec<u8>,
) {
    if !bytes.starts_with(b"\x7fELF") {
        return;
    }
    let digest = format!("{:x}", Sha256::digest(&bytes));
    match scan_elf(&bytes, &digest) {
        Ok(None) => {}
        Ok(Some(sites)) => {
            found.extend(sites.iter().cloned());
            report.executables.push(ExecutableScan {
                path: image_path.to_owned(),
                digest,
                sites,
            });
        }
        Err(error) => report.scan_errors.push(format!("{image_path}: {error}")),
    }
}

fn finish_scan(
    report: &mut ImageReport,
    found: &BTreeSet<InstructionSite>,
    reviewed: &BTreeMap<InstructionSite, String>,
) {
    for site in found {
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
    let path = root.join(ALLOWLIST_PATH);
    if !path.try_exists()? {
        return Ok(BTreeMap::new());
    }
    let path = rooted_file(root, ALLOWLIST_PATH).ok_or("instruction allowlist leaves the image")?;
    parse_allowlist(&fs::read_to_string(path)?)
}

fn parse_allowlist(text: &str) -> Result<BTreeMap<InstructionSite, String>, Box<dyn Error>> {
    let mut sites = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
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
    parse_attestation(&text, report, |path| {
        let executable = rooted_file(root, path.trim_start_matches('/'))
            .ok_or("attested executable is missing or leaves the image")?;
        Ok(fs::read(executable)?)
    });
}

fn parse_attestation<F, R>(text: &str, report: &mut ImageReport, mut file: F)
where
    F: FnMut(&str) -> Result<R, Box<dyn Error>>,
    R: AsRef<[u8]>,
{
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
            let digest = format!("{:x}", Sha256::digest(file(path)?.as_ref()));
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

fn x86_host_input_name(mnemonic: Mnemonic) -> Option<&'static str> {
    match mnemonic {
        Mnemonic::Rdrand => Some("RDRAND"),
        Mnemonic::Rdseed => Some("RDSEED"),
        Mnemonic::Rdpid => Some("RDPID"),
        Mnemonic::Lsl => Some("LSL"),
        _ => None,
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
    let writable_executable = object::elf::PF_W | object::elf::PF_X;
    let mut sites = Vec::new();
    let mut executable_segments = 0;
    let mut x86_executable_loads = Vec::new();
    for (header_index, header) in elf.elf_program_headers().iter().enumerate() {
        let flags = header.p_flags(elf.endian());
        match header.p_type(elf.endian()) {
            object::elf::PT_GNU_STACK if flags & object::elf::PF_X != 0 => {
                return Err("executable ELF stack".into());
            }
            object::elf::PT_LOAD if flags & writable_executable == writable_executable => {
                return Err("writable executable ELF segment".into());
            }
            object::elf::PT_LOAD if flags & object::elf::PF_X != 0 => {
                executable_segments += 1;
                let start = header.p_vaddr(elf.endian());
                let file_size = header.p_filesz(elf.endian());
                if file_size == 0 || file_size > header.p_memsz(elf.endian()) {
                    return Err("invalid executable ELF segment size".into());
                }
                let end = start
                    .checked_add(file_size)
                    .ok_or("executable ELF segment address overflow")?;
                let segment_code = header
                    .data(elf.endian(), bytes)
                    .map_err(|()| "executable ELF segment outside file")?;
                let file_offset = usize::try_from(header.p_offset(elf.endian()))?;
                let page_head = usize::try_from(start % 4096)?;
                if file_offset % 4096 != page_head {
                    return Err("executable ELF segment page offset mismatch".into());
                }
                let mapped_start = start - u64::try_from(page_head)?;
                let file_start = file_offset
                    .checked_sub(page_head)
                    .ok_or("executable ELF file offset underflow")?;
                let file_end = file_offset
                    .checked_add(segment_code.len())
                    .ok_or("executable ELF file offset overflow")?;
                let mut code = bytes[file_start..file_end].to_vec();
                let page_tail = usize::try_from((4096 - end % 4096) % 4096)?;
                end.checked_add(u64::try_from(page_tail)?)
                    .ok_or("executable ELF mapped address overflow")?;
                let available = bytes.len().saturating_sub(file_end).min(page_tail);
                code.extend_from_slice(&bytes[file_end..file_end + available]);
                let mapped_len = code
                    .len()
                    .checked_add(page_tail - available)
                    .ok_or("executable ELF mapped size overflow")?;
                code.resize(mapped_len, 0);
                let mut mapped_sections = 0;
                let mut mapped_ranges = Vec::new();
                for section in elf.sections() {
                    let object::SectionFlags::Elf { sh_flags } = section.flags() else {
                        continue;
                    };
                    if sh_flags & u64::from(object::elf::SHF_ALLOC) == 0 || section.size() == 0 {
                        continue;
                    }
                    let section_end = section
                        .address()
                        .checked_add(section.size())
                        .ok_or("mapped ELF section address overflow")?;
                    if section.address() >= end || section_end <= start {
                        continue;
                    }
                    let Some(section_code) = header
                        .data_range(elf.endian(), bytes, section.address(), section.size())
                        .map_err(|()| "mapped ELF section outside file")?
                    else {
                        return Err("partially mapped executable ELF section".into());
                    };
                    mapped_sections += 1;
                    if elf.architecture() == object::Architecture::Aarch64
                        && sh_flags & u64::from(object::elf::SHF_EXECINSTR) == 0
                    {
                        return Err("AArch64 executable load contains an allocated non-executable section; link with separate code pages".into());
                    }
                    if elf.architecture() == object::Architecture::X86_64 {
                        let offset = usize::try_from(section.address() - mapped_start)?;
                        let section_end = offset
                            .checked_add(section_code.len())
                            .ok_or("mapped ELF section size overflow")?;
                        mapped_ranges.push((offset, section_end));
                    }
                }
                if mapped_sections == 0 {
                    return Err("executable ELF segment without a mapped section".into());
                }
                if elf.architecture() == object::Architecture::Aarch64 {
                    let page_bytes = 4096_u64;
                    let offset = header.p_offset(elf.endian());
                    if offset % page_bytes != start % page_bytes {
                        return Err("unaligned AArch64 ELF load mapping".into());
                    }
                    let mapped_file_start = offset / page_bytes * page_bytes;
                    let mapped_file_end = offset
                        .checked_add(file_size)
                        .and_then(|end| end.checked_add(page_bytes - 1))
                        .ok_or("executable ELF file-page overflow")?
                        / page_bytes
                        * page_bytes;
                    let mapped_address = start
                        .checked_sub(offset - mapped_file_start)
                        .ok_or("executable ELF page address underflow")?;
                    let end_address = start
                        .checked_add(header.p_memsz(elf.endian()))
                        .and_then(|end| end.checked_add(page_bytes - 1))
                        .ok_or("executable ELF memory-page overflow")?
                        / page_bytes
                        * page_bytes;
                    for (other_index, other) in elf.elf_program_headers().iter().enumerate() {
                        if other_index == header_index
                            || other.p_type(elf.endian()) != object::elf::PT_LOAD
                        {
                            continue;
                        }
                        let other_offset = other.p_offset(elf.endian());
                        let other_address = other.p_vaddr(elf.endian());
                        if other_offset % page_bytes != other_address % page_bytes {
                            return Err("unaligned loaded AArch64 ELF mapping".into());
                        }
                        let other_memory_size = other.p_memsz(elf.endian());
                        let other_memory_start = other_address / page_bytes * page_bytes;
                        let other_memory_end = if other_memory_size == 0 {
                            other_memory_start
                        } else {
                            other_address
                                .checked_add(other_memory_size)
                                .and_then(|end| end.checked_add(page_bytes - 1))
                                .ok_or("loaded ELF memory-page overflow")?
                                / page_bytes
                                * page_bytes
                        };
                        if other_memory_start < end_address && mapped_address < other_memory_end {
                            return Err("executable AArch64 ELF memory-page alias".into());
                        }
                    }
                    let mapped_file_end = mapped_file_end.min(bytes.len() as u64);
                    let mapped_bytes = &bytes
                        [usize::try_from(mapped_file_start)?..usize::try_from(mapped_file_end)?];
                    for (address, instruction) in
                        scan_code(elf.architecture(), mapped_address, mapped_bytes)?
                    {
                        sites.push(InstructionSite {
                            digest: digest.to_owned(),
                            address,
                            instruction: instruction.into(),
                        });
                    }
                } else {
                    mapped_ranges.sort_unstable();
                    let mut anchors = BTreeSet::from([0, page_head]);
                    let mut boundaries = BTreeSet::new();
                    for (range_start, range_end) in mapped_ranges {
                        anchors.insert(range_start);
                        boundaries.insert(range_start);
                        if range_end < code.len() {
                            anchors.insert(range_end);
                            boundaries.insert(range_end);
                        }
                    }
                    x86_executable_loads.push((mapped_start, code, anchors, boundaries));
                }
            }
            _ => {}
        }
    }
    if executable_segments == 0 {
        return Err("no executable ELF segment".into());
    }
    x86_executable_loads.sort_unstable_by_key(|(start, _, _, _)| *start);
    for pair in x86_executable_loads.windows(2) {
        let (left_start, left, _, _) = &pair[0];
        let (right_start, _, _, _) = &pair[1];
        let left_end = left_start
            .checked_add(u64::try_from(left.len())?)
            .ok_or("executable ELF mapped address overflow")?;
        if left_end > *right_start {
            return Err("overlapping executable ELF mappings".into());
        }
    }
    let mut group_start: Option<u64> = None;
    let mut group_code = Vec::new();
    let mut group_anchors = BTreeSet::new();
    let mut group_boundaries = BTreeSet::new();
    for (start, code, anchors, boundaries) in x86_executable_loads {
        if let Some(first) = group_start
            && first.checked_add(u64::try_from(group_code.len())?) != Some(start)
        {
            push_x86_sites(
                first,
                &group_code,
                &group_anchors,
                &group_boundaries,
                digest,
                &mut sites,
            )?;
            group_start = None;
            group_code.clear();
            group_anchors.clear();
            group_boundaries.clear();
        }
        if group_start.is_none() {
            group_start = Some(start);
        }
        let offset = group_code.len();
        group_code.extend_from_slice(&code);
        group_anchors.insert(offset);
        group_boundaries.insert(offset);
        for anchor in anchors {
            group_anchors.insert(offset + anchor);
        }
        for boundary in boundaries {
            group_boundaries.insert(offset + boundary);
        }
    }
    if let Some(first) = group_start {
        push_x86_sites(
            first,
            &group_code,
            &group_anchors,
            &group_boundaries,
            digest,
            &mut sites,
        )?;
    }
    sites.sort();
    sites.dedup();
    Ok(Some(sites))
}

fn push_x86_sites(
    start: u64,
    code: &[u8],
    anchors: &BTreeSet<usize>,
    boundaries: &BTreeSet<usize>,
    digest: &str,
    sites: &mut Vec<InstructionSite>,
) -> Result<(), Box<dyn Error>> {
    for &boundary in boundaries {
        if boundary == 0 || boundary >= code.len() {
            continue;
        }
        for candidate_start in boundary.saturating_sub(14)..boundary {
            let address = start
                .checked_add(u64::try_from(candidate_start)?)
                .ok_or("executable ELF segment address overflow")?;
            let mut decoder =
                Decoder::with_ip(64, &code[candidate_start..], address, DecoderOptions::NONE);
            let instruction = decoder.decode();
            if let Some(name) = x86_host_input_name(instruction.mnemonic())
                && candidate_start + instruction.len() > boundary
            {
                sites.push(InstructionSite {
                    digest: digest.to_owned(),
                    address,
                    instruction: name.into(),
                });
            }
        }
    }
    for &offset in anchors {
        if offset >= code.len() {
            continue;
        }
        let address = start
            .checked_add(u64::try_from(offset)?)
            .ok_or("executable ELF segment address overflow")?;
        for (address, instruction) in
            scan_code(object::Architecture::X86_64, address, &code[offset..])?
        {
            sites.push(InstructionSite {
                digest: digest.to_owned(),
                address,
                instruction: instruction.into(),
            });
        }
    }
    Ok(())
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
            if let Some(name) = x86_host_input_name(instruction.mnemonic()) {
                sites.push((instruction.ip(), name));
            }
        }
    } else {
        if !address.is_multiple_of(4) {
            return Err("unaligned AArch64 executable segment".into());
        }
        for (index, bytes) in data.chunks_exact(4).enumerate() {
            let word = u32::from_le_bytes(bytes.try_into()?);
            let site = address
                .checked_add((index as u64) * 4)
                .ok_or("instruction address overflow")?;
            if is_arm_llsc_exclusive(word) {
                return Err(format!("unsupported AArch64 LL/SC instruction at 0x{site:x}").into());
            }
            let name = match word & !0x1f {
                0xd53b2400 => "RNDR",
                0xd53b2420 => "RNDRRS",
                _ => continue,
            };
            sites.push((site, name));
        }
    }
    Ok(sites)
}

fn is_arm_llsc_exclusive(word: u32) -> bool {
    word & 0x3f80_0000 == 0x0800_0000 && (word & 0x0020_0000 == 0 || word >> 30 >= 2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oci_support::image::{Ownership, RuntimeConfig, StagedImage};

    fn elf(code: &[u8], load_flags: u32, stack_flags: u32) -> Vec<u8> {
        let mut bytes = vec![0; 4097 + code.len()];
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
            (72, 4096_u64),
            (80, 0x1000),
            (96, code.len() as u64),
            (104, code.len() as u64),
            (112, 4096),
        ] {
            bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        bytes[120..124].copy_from_slice(&object::elf::PT_GNU_STACK.to_le_bytes());
        bytes[124..128].copy_from_slice(&stack_flags.to_le_bytes());
        bytes[260..264].copy_from_slice(&object::elf::SHT_PROGBITS.to_le_bytes());
        for (offset, value) in [
            (264, 6_u64),
            (272, 0x1000),
            (280, 4096),
            (288, code.len() as u64),
            (304, 1),
        ] {
            bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        bytes[324..328].copy_from_slice(&object::elf::SHT_STRTAB.to_le_bytes());
        bytes[344..352].copy_from_slice(&(4096_u64 + code.len() as u64).to_le_bytes());
        bytes[352..360].copy_from_slice(&1_u64.to_le_bytes());
        bytes[368..376].copy_from_slice(&1_u64.to_le_bytes());
        bytes[4096..4096 + code.len()].copy_from_slice(code);
        bytes
    }

    fn adjacent_x86_elf(left_tail: &[u8], right: &[u8]) -> Vec<u8> {
        let mut bytes = elf(&vec![0x90; 4096], 5, 6);
        bytes.resize(8193 + right.len(), 0);
        bytes[56..58].copy_from_slice(&3_u16.to_le_bytes());
        bytes[60..62].copy_from_slice(&4_u16.to_le_bytes());
        bytes[62..64].copy_from_slice(&3_u16.to_le_bytes());
        bytes[120..124].copy_from_slice(&object::elf::PT_LOAD.to_le_bytes());
        bytes[124..128].copy_from_slice(&5_u32.to_le_bytes());
        for (offset, value) in [
            (128, 8192_u64),
            (136, 0x2000),
            (144, 0x2000),
            (152, right.len() as u64),
            (160, right.len() as u64),
            (168, 4096),
        ] {
            bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        bytes[176..180].copy_from_slice(&object::elf::PT_GNU_STACK.to_le_bytes());
        bytes[180..184].copy_from_slice(&6_u32.to_le_bytes());
        bytes[324..328].copy_from_slice(&object::elf::SHT_PROGBITS.to_le_bytes());
        for (offset, value) in [
            (328, 6_u64),
            (336, 0x2000),
            (344, 8192),
            (352, right.len() as u64),
            (368, 1),
        ] {
            bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        bytes[388..392].copy_from_slice(&object::elf::SHT_STRTAB.to_le_bytes());
        bytes[408..416].copy_from_slice(&(8192_u64 + right.len() as u64).to_le_bytes());
        bytes[416..424].copy_from_slice(&1_u64.to_le_bytes());
        bytes[432..440].copy_from_slice(&1_u64.to_le_bytes());
        bytes[8192 - left_tail.len()..8192].copy_from_slice(left_tail);
        bytes[8192..8192 + right.len()].copy_from_slice(right);
        bytes
    }

    fn assert_x86_lsl_needs_vm_review_and_blocks_uml(bytes: &[u8], address: u64) {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("probe"), bytes).unwrap();
        let report = inspect_root(root.path()).unwrap();
        assert!(report.scan_errors.iter().any(|error| error.contains("LSL")));
        assert_eq!(
            report.executables[0]
                .sites
                .iter()
                .map(|site| (site.address, site.instruction.as_str()))
                .collect::<Vec<_>>(),
            [(address, "LSL")]
        );
        assert!(report.require_for(ExecutionMode::Vm).is_err());
        assert!(report.require_for(ExecutionMode::Uml).is_err());
        fs::create_dir_all(root.path().join("etc/harmony")).unwrap();
        fs::write(
            root.path().join(ALLOWLIST_PATH),
            format!(
                "{:x} 0x{address:x} LSL reviewed executable bytes\n",
                Sha256::digest(bytes)
            ),
        )
        .unwrap();
        let reviewed = inspect_root(root.path()).unwrap();
        assert!(reviewed.require_for(ExecutionMode::Vm).is_ok());
        assert!(reviewed.require_for(ExecutionMode::Uml).is_err());
    }

    fn arm_elf(code: &[u8], declared_size: usize) -> Vec<u8> {
        let mut bytes = elf(code, 5, 6);
        bytes[18..20].copy_from_slice(&object::elf::EM_AARCH64.to_le_bytes());
        bytes.resize(4096 + code.len() + 1, 0);
        bytes[4096..4096 + code.len()].copy_from_slice(code);
        bytes[80..88].copy_from_slice(&0x1000_u64.to_le_bytes());
        bytes[72..80].copy_from_slice(&4096_u64.to_le_bytes());
        bytes[96..104].copy_from_slice(&(declared_size as u64).to_le_bytes());
        bytes[104..112].copy_from_slice(&(declared_size as u64).to_le_bytes());
        bytes[280..288].copy_from_slice(&4096_u64.to_le_bytes());
        bytes[288..296].copy_from_slice(&(declared_size as u64).to_le_bytes());
        bytes[344..352].copy_from_slice(&(4096_u64 + code.len() as u64).to_le_bytes());
        bytes
    }

    fn arm_elf_with_data_load(mut bytes: Vec<u8>, offset: u64, address: u64) -> Vec<u8> {
        bytes[120..124].copy_from_slice(&object::elf::PT_LOAD.to_le_bytes());
        bytes[124..128].copy_from_slice(&(object::elf::PF_R | object::elf::PF_W).to_le_bytes());
        bytes[128..136].copy_from_slice(&offset.to_le_bytes());
        bytes[136..144].copy_from_slice(&address.to_le_bytes());
        bytes[144..152].copy_from_slice(&address.to_le_bytes());
        bytes[152..160].copy_from_slice(&4_u64.to_le_bytes());
        bytes[160..168].copy_from_slice(&4_u64.to_le_bytes());
        bytes[168..176].copy_from_slice(&4096_u64.to_le_bytes());
        bytes.resize(bytes.len().max(offset as usize + 4), 0);
        bytes
    }

    fn arm_elf_with_readonly_data_in_code_load() -> Vec<u8> {
        let code = [0xd65f03c0_u32.to_le_bytes(), *b"PATH"].concat();
        let mut bytes = arm_elf(&code, code.len());
        bytes[60..62].copy_from_slice(&4_u16.to_le_bytes());
        bytes[288..296].copy_from_slice(&4_u64.to_le_bytes());
        bytes[388..392].copy_from_slice(&object::elf::SHT_PROGBITS.to_le_bytes());
        bytes[392..400].copy_from_slice(&u64::from(object::elf::SHF_ALLOC).to_le_bytes());
        bytes[400..408].copy_from_slice(&0x1004_u64.to_le_bytes());
        bytes[408..416].copy_from_slice(&4100_u64.to_le_bytes());
        bytes[416..424].copy_from_slice(&4_u64.to_le_bytes());
        bytes[432..440].copy_from_slice(&4_u64.to_le_bytes());
        bytes
    }

    #[test]
    fn arm_exclusive_retry_sequence_fails_public_vm_admission() {
        let code = [0x885f7c20_u32, 0x88027c20, 0xd65f03c0]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>();
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("probe"), arm_elf(&code, code.len())).unwrap();
        let report = inspect_root(root.path()).unwrap();
        assert!(
            report
                .scan_errors
                .iter()
                .any(|error| error.contains("LL/SC"))
        );
        assert!(report.require_for(ExecutionMode::Vm).is_err());
    }

    #[test]
    fn arm_exclusive_mask_keeps_lse_and_acquire_release_loads() {
        for word in [0x885f7c20, 0x88027c20, 0x085f7c61, 0xc87f7c20] {
            assert!(is_arm_llsc_exclusive(word), "{word:08x}");
        }
        for word in [0x48207c82, 0x4860fc82, 0xc8dffc00] {
            assert!(!is_arm_llsc_exclusive(word), "{word:08x}");
            assert!(
                scan_code(object::Architecture::Aarch64, 0x1000, &word.to_le_bytes())
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn arm_exclusive_in_executable_page_tail_fails_public_vm_admission() {
        let code = [0xd65f03c0_u32, 0x885f7c20]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>();
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("probe"), arm_elf(&code, 4)).unwrap();
        let report = inspect_root(root.path()).unwrap();
        assert!(
            report
                .scan_errors
                .iter()
                .any(|error| error.contains("LL/SC"))
        );
        assert!(report.require_for(ExecutionMode::Vm).is_err());
    }

    #[test]
    fn arm_entropy_in_executable_page_tail_needs_review() {
        let code = [0xd65f03c0_u32, 0xd53b2400]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>();
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("probe"), arm_elf(&code, 4)).unwrap();
        let report = inspect_root(root.path()).unwrap();
        assert!(
            report
                .scan_errors
                .iter()
                .any(|error| error.contains("RNDR"))
        );
        assert!(report.require_for(ExecutionMode::Vm).is_err());
    }

    #[test]
    fn arm_readonly_data_in_executable_load_fails_clearly() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("probe"),
            arm_elf_with_readonly_data_in_code_load(),
        )
        .unwrap();
        let report = inspect_root(root.path()).unwrap();
        assert!(
            report
                .scan_errors
                .iter()
                .any(|error| error.contains("allocated non-executable section"))
        );
        assert!(report.require_for(ExecutionMode::Vm).is_err());
    }

    #[test]
    fn arm_file_page_reuse_at_distinct_memory_pages_is_admitted() {
        let ret = 0xd65f03c0_u32.to_le_bytes();
        let bytes = arm_elf_with_data_load(arm_elf(&ret, ret.len()), 4100, 0x3004);
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("probe"), bytes).unwrap();
        let report = inspect_root(root.path()).unwrap();
        assert!(report.scan_errors.is_empty(), "{:?}", report.scan_errors);
        assert!(report.require_for(ExecutionMode::Vm).is_ok());
    }

    #[test]
    fn arm_executable_memory_pages_cannot_alias_a_loaded_data_page() {
        let ret = 0xd65f03c0_u32.to_le_bytes();
        let bytes = arm_elf_with_data_load(arm_elf(&ret, ret.len()), 8196, 0x1004);
        let error = scan_elf(&bytes, "probe").unwrap_err().to_string();
        assert!(error.contains("memory-page alias"), "{error}");
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
        assert!(accepted.require_for(ExecutionMode::Vm).is_ok());
        assert!(
            accepted
                .require_for(ExecutionMode::Uml)
                .unwrap_err()
                .to_string()
                .contains("RDRAND")
        );
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
    #[cfg(unix)]
    fn archived_admission_matches_the_exact_packed_files_and_symlink_aliases() {
        let root = tempfile::tempdir().unwrap();
        for directory in ["bin", "etc/harmony", "symbols", "usr/lib"] {
            fs::create_dir_all(root.path().join(directory)).unwrap();
        }
        let executable = elf(&[0x0f, 0xc7, 0xf0, 0xc3], 5, 6);
        let digest = format!("{:x}", Sha256::digest(&executable));
        fs::write(root.path().join("bin/node"), &executable).unwrap();
        fs::write(
            root.path().join("etc/harmony/reviewed"),
            format!("{digest} 0x1000 RDRAND guarded\n"),
        )
        .unwrap();
        std::os::unix::fs::symlink("reviewed", root.path().join(ALLOWLIST_PATH)).unwrap();
        fs::write(
            root.path().join("symbols/events"),
            format!("{digest}  /bin/node\n"),
        )
        .unwrap();
        std::os::unix::fs::symlink("events", root.path().join(ATTESTATION_PATH)).unwrap();
        fs::write(root.path().join("symbols/node.sym.tsv"), b"1\tnode.rs:1\n").unwrap();
        fs::write(root.path().join("usr/lib/libvoidstar.so"), b"runtime").unwrap();
        let staged = StagedImage {
            rootfs: root.path().to_path_buf(),
            owners: Ownership::default(),
            config: RuntimeConfig::default(),
        };
        let prepared =
            oci_support::bundle::prepare(&staged, &oci_support::bundle::LaunchRequest::default())
                .unwrap();
        let archive = prepared.archived_rootfs().unwrap();
        let packed = ArchivedImage::new(&archive).inspect().unwrap();
        let staged = inspect_root(root.path()).unwrap();
        assert_eq!(
            serde_json::to_value(&packed).unwrap(),
            serde_json::to_value(&staged).unwrap()
        );
        assert!(packed.instrumented_events());
        assert!(packed.require_for(ExecutionMode::Vm).is_ok());
        assert!(packed.require_for(ExecutionMode::Uml).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn archived_admission_treats_missing_metadata_under_symlinks_as_absent() {
        let root = tempfile::tempdir().unwrap();
        for directory in ["usr/share/symbols", "real/etc/harmony"] {
            fs::create_dir_all(root.path().join(directory)).unwrap();
        }
        std::os::unix::fs::symlink("usr/share/symbols", root.path().join("symbols")).unwrap();
        std::os::unix::fs::symlink("real/etc", root.path().join("etc")).unwrap();
        std::os::unix::fs::symlink(
            "missing-review",
            root.path().join("real/etc/harmony/instruction-allowlist"),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            "missing-attestation",
            root.path()
                .join("usr/share/symbols/harmony-instrumented-events"),
        )
        .unwrap();
        let staged = StagedImage {
            rootfs: root.path().to_path_buf(),
            owners: Ownership::default(),
            config: RuntimeConfig::default(),
        };
        let prepared =
            oci_support::bundle::prepare(&staged, &oci_support::bundle::LaunchRequest::default())
                .unwrap();
        let archive = prepared.archived_rootfs().unwrap();
        let packed = ArchivedImage::new(&archive).inspect().unwrap();
        let staged = inspect_root(root.path()).unwrap();
        assert_eq!(
            serde_json::to_value(&packed).unwrap(),
            serde_json::to_value(&staged).unwrap()
        );
        assert!(packed.require_for(ExecutionMode::Vm).is_ok());
    }

    #[test]
    fn uml_rejects_reviewed_rdpid_because_the_host_cpu_id_varies() {
        assert_eq!(
            scan_code(
                object::Architecture::X86_64,
                0x2000,
                &[0xf3, 0x48, 0x0f, 0xc7, 0xf8]
            )
            .unwrap(),
            [(0x2000, "RDPID")]
        );
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("etc/harmony")).unwrap();
        let bytes = elf(&[0xf3, 0x0f, 0xc7, 0xf8, 0xc3], 5, 6);
        fs::write(root.path().join("node"), &bytes).unwrap();
        assert_eq!(
            inspect_root(root.path()).unwrap().executables[0].sites[0].instruction,
            "RDPID"
        );
        fs::write(
            root.path().join(ALLOWLIST_PATH),
            format!(
                "{:x} 0x1000 RDPID native host CPU id\n",
                Sha256::digest(&bytes)
            ),
        )
        .unwrap();
        let reviewed = inspect_root(root.path()).unwrap();
        assert!(reviewed.require_for(ExecutionMode::Vm).is_ok());
        assert!(
            reviewed
                .require_for(ExecutionMode::Uml)
                .unwrap_err()
                .to_string()
                .contains("RDPID")
        );
    }

    #[test]
    fn uml_rejects_reviewed_lsl_because_the_host_cpu_id_varies() {
        assert_eq!(
            scan_code(object::Architecture::X86_64, 0x2000, &[0x0f, 0x03, 0xc1]).unwrap(),
            [(0x2000, "LSL")]
        );
        assert_eq!(
            scan_code(
                object::Architecture::X86_64,
                0x2000,
                &[0x48, 0x0f, 0x03, 0xc1]
            )
            .unwrap(),
            [(0x2000, "LSL")]
        );
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("etc/harmony")).unwrap();
        let bytes = elf(&[0xb9, 0x7b, 0, 0, 0, 0x48, 0x0f, 0x03, 0xc1, 0xc3], 5, 6);
        fs::write(root.path().join("node"), &bytes).unwrap();
        let unreviewed = inspect_root(root.path()).unwrap();
        assert!(
            unreviewed
                .scan_errors
                .iter()
                .any(|error| error.contains("LSL"))
        );
        assert_eq!(unreviewed.executables[0].sites[0].instruction, "LSL");
        fs::write(
            root.path().join(ALLOWLIST_PATH),
            format!(
                "{:x} 0x1005 LSL native host CPU id\n",
                Sha256::digest(&bytes)
            ),
        )
        .unwrap();
        let reviewed = inspect_root(root.path()).unwrap();
        assert!(reviewed.require_for(ExecutionMode::Vm).is_ok());
        assert!(
            reviewed
                .require_for(ExecutionMode::Uml)
                .unwrap_err()
                .to_string()
                .contains("LSL")
        );
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
        let sites = scan_elf(&elf(&code, 5, 6), "").unwrap().unwrap();
        assert_eq!(
            sites.iter().map(|site| site.address).collect::<Vec<_>>(),
            [0x100c, 0x1010]
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
        let bytes = arm_elf(&code, code.len());
        let sites = scan_elf(&bytes, "").unwrap().unwrap();
        assert_eq!(
            sites.iter().map(|site| site.address).collect::<Vec<_>>(),
            [0x1000, 0x1004]
        );
    }

    #[test]
    fn executable_segments_without_mapped_sections_fail() {
        let mut stripped = elf(&[0x0f, 0xc7, 0xf0], 5, 6);
        stripped[40..48].copy_from_slice(&0_u64.to_le_bytes());
        stripped[60..64].copy_from_slice(&0_u32.to_le_bytes());
        assert!(scan_elf(&stripped, "").is_err());
        let mut data = elf(&[0x0f, 0xc7, 0xf0], 5, 6);
        data[264..272].copy_from_slice(&2_u64.to_le_bytes());
        assert_eq!(
            scan_elf(&data, "").unwrap().unwrap()[0].instruction,
            "RDRAND"
        );
    }

    #[test]
    fn executable_segment_bytes_hidden_by_section_metadata_fail_admission() {
        let root = tempfile::tempdir().unwrap();
        let mut bytes = elf(&[0x0f, 0xc7, 0xf0, 0xc3], 5, 6);
        bytes[272..280].copy_from_slice(&0x1003_u64.to_le_bytes());
        bytes[280..288].copy_from_slice(&4099_u64.to_le_bytes());
        bytes[288..296].copy_from_slice(&1_u64.to_le_bytes());
        fs::write(root.path().join("node"), bytes).unwrap();

        let report = inspect_root(root.path()).unwrap();
        assert_eq!(report.executables[0].sites[0].instruction, "RDRAND");
        assert_eq!(report.executables[0].sites[0].address, 0x1000);
        assert!(
            report.require_for(ExecutionMode::Uml).is_err(),
            "{report:?}"
        );
    }

    #[test]
    fn mapped_section_starts_preserve_x86_decoding_after_segment_headers() {
        let mut bytes = elf(&[0x0f, 0xc7, 0xf0, 0xc3], 5, 6);
        for (offset, value) in [
            (24, 0x401000_u64),
            (72, 4095),
            (80, 0x400fff),
            (88, 0x400fff),
            (96, 5),
            (104, 5),
            (272, 0x401000),
        ] {
            bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        bytes[4095] = 0xe8;
        assert!(
            scan_code(object::Architecture::X86_64, 0x400fff, &bytes[4095..4100])
                .unwrap()
                .is_empty()
        );
        let sites = scan_elf(&bytes, "").unwrap().unwrap();
        assert_eq!(
            sites
                .iter()
                .map(|site| (site.address, site.instruction.as_str()))
                .collect::<Vec<_>>(),
            [(0x401000, "RDRAND")]
        );
    }

    #[test]
    fn x86_segment_prelude_can_fall_through_into_a_different_section_decode() {
        let mut bytes = elf(&[0xb8, 0x90, 0x0f, 0xc7, 0xf0, 0xc3], 5, 6);
        for (offset, value) in [
            (24, 0x401000_u64),
            (72, 4095),
            (80, 0x400fff),
            (88, 0x400fff),
            (96, 7),
            (104, 7),
            (272, 0x401000),
        ] {
            bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        bytes[4095] = 0x3c;
        assert_eq!(
            scan_code(object::Architecture::X86_64, 0x400fff, &bytes[4095..4102])
                .unwrap()
                .len(),
            1
        );
        let sites = scan_elf(&bytes, "").unwrap().unwrap();
        assert_eq!(
            sites
                .iter()
                .map(|site| (site.address, site.instruction.as_str()))
                .collect::<Vec<_>>(),
            [(0x401002, "RDRAND")]
        );
    }

    #[test]
    fn x86_host_input_split_across_mapped_section_boundary_is_reviewable() {
        for (mnemonic, opcode, modrm) in [
            ("RDRAND", 0xc7, 0xf0),
            ("RDSEED", 0xc7, 0xf8),
            ("LSL", 0x03, 0xc1),
        ] {
            for section_size in [1_u64, 2] {
                let mut bytes = elf(&[0x0f, opcode, modrm, 0xc3], 5, 6);
                bytes[288..296].copy_from_slice(&section_size.to_le_bytes());
                let sites = scan_elf(&bytes, "").unwrap().unwrap();
                assert_eq!(
                    sites
                        .iter()
                        .map(|site| (site.address, site.instruction.as_str()))
                        .collect::<Vec<_>>(),
                    [(0x1000, mnemonic)],
                    "{mnemonic} crossing section end {section_size}"
                );
            }
            let mut bytes = elf(&[0x0f, opcode, modrm, 0xc3], 5, 6);
            bytes[272..280].copy_from_slice(&0x1002_u64.to_le_bytes());
            bytes[280..288].copy_from_slice(&4098_u64.to_le_bytes());
            bytes[288..296].copy_from_slice(&2_u64.to_le_bytes());
            let sites = scan_elf(&bytes, "").unwrap().unwrap();
            assert_eq!(
                sites
                    .iter()
                    .map(|site| (site.address, site.instruction.as_str()))
                    .collect::<Vec<_>>(),
                [(0x1000, mnemonic)],
                "{mnemonic} crossing section start"
            );
        }
    }

    #[test]
    fn x86_lsl_split_across_misaligned_section_boundary_needs_review() {
        let code = [
            0xb9, 0x7b, 0, 0, 0, 0xeb, 0x01, 0xb0, 0x0f, 0x03, 0xc1, 0x89, 0xc7, 0xb8, 0x3c, 0, 0,
            0, 0x0f, 0x05,
        ];
        let mut bytes = elf(&code, 5, 6);
        bytes[60..62].copy_from_slice(&4_u16.to_le_bytes());
        bytes[288..296].copy_from_slice(&9_u64.to_le_bytes());
        bytes[388..392].copy_from_slice(&object::elf::SHT_PROGBITS.to_le_bytes());
        bytes[392..400].copy_from_slice(
            &(u64::from(object::elf::SHF_ALLOC) | u64::from(object::elf::SHF_EXECINSTR))
                .to_le_bytes(),
        );
        bytes[400..408].copy_from_slice(&0x1009_u64.to_le_bytes());
        bytes[408..416].copy_from_slice(&4105_u64.to_le_bytes());
        bytes[416..424].copy_from_slice(&(code.len() as u64 - 9).to_le_bytes());
        bytes[432..440].copy_from_slice(&1_u64.to_le_bytes());
        let sites = scan_elf(&bytes, "probe").unwrap().unwrap();
        assert_eq!(
            sites
                .iter()
                .map(|site| (site.address, site.instruction.as_str()))
                .collect::<Vec<_>>(),
            [(0x1008, "LSL")]
        );
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("probe"), &bytes).unwrap();
        let report = inspect_root(root.path()).unwrap();
        assert!(report.require_for(ExecutionMode::Vm).is_err());
        assert!(report.require_for(ExecutionMode::Uml).is_err());
        fs::create_dir_all(root.path().join("etc/harmony")).unwrap();
        fs::write(
            root.path().join(ALLOWLIST_PATH),
            format!(
                "{:x} 0x1008 LSL native host CPU identity\n",
                Sha256::digest(&bytes)
            ),
        )
        .unwrap();
        let reviewed = inspect_root(root.path()).unwrap();
        assert!(reviewed.require_for(ExecutionMode::Vm).is_ok());
        assert!(reviewed.require_for(ExecutionMode::Uml).is_err());
    }

    #[test]
    fn x86_memory_lsl_split_after_modrm_or_sib_across_sections_needs_review() {
        for (code, boundary) in [
            (
                vec![
                    0x6a, 0x7b, 0xeb, 0x01, 0xb0, 0x0f, 0x03, 0x04, 0x24, 0x89, 0xc7, 0xb8, 0x3c,
                    0, 0, 0, 0x0f, 0x05,
                ],
                8_usize,
            ),
            (
                vec![
                    0x6a, 0x7b, 0xeb, 0x01, 0xb0, 0x0f, 0x03, 0x44, 0x24, 0x08, 0x89, 0xc7, 0xb8,
                    0x3c, 0, 0, 0, 0x0f, 0x05,
                ],
                9,
            ),
        ] {
            let mut bytes = elf(&code, 5, 6);
            bytes[60..62].copy_from_slice(&4_u16.to_le_bytes());
            bytes[288..296].copy_from_slice(&(boundary as u64).to_le_bytes());
            bytes[388..392].copy_from_slice(&object::elf::SHT_PROGBITS.to_le_bytes());
            for (offset, value) in [
                (392, 6_u64),
                (400, 0x1000 + boundary as u64),
                (408, 4096 + boundary as u64),
                (416, (code.len() - boundary) as u64),
                (432, 1),
            ] {
                bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
            }
            assert_x86_lsl_needs_vm_review_and_blocks_uml(&bytes, 0x1005);
        }
    }

    #[test]
    fn x86_prefixed_lsl_with_full_displacement_uses_the_whole_boundary_lookback() {
        let code = [
            0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x0f, 0x03, 0x84, 0x24, 0, 0, 0, 0,
        ];
        let mut sites = Vec::new();
        push_x86_sites(
            0x1000,
            &code,
            &BTreeSet::new(),
            &BTreeSet::from([14]),
            "probe",
            &mut sites,
        )
        .unwrap();
        assert_eq!(
            sites
                .first()
                .map(|site| (site.address, site.instruction.as_str())),
            Some((0x1000, "LSL"))
        );
        assert!(sites.iter().all(|site| site.instruction == "LSL"));
    }

    #[test]
    fn x86_memory_lsl_completed_in_executable_page_tail_needs_review() {
        let code = [
            0x6a, 0x7b, 0xeb, 0x01, 0xb0, 0x0f, 0x03, 0x04, 0x24, 0x89, 0xc7, 0xb8, 0x3c, 0, 0, 0,
            0x0f, 0x05,
        ];
        let mut bytes = elf(&code, 5, 6);
        bytes[96..104].copy_from_slice(&8_u64.to_le_bytes());
        bytes[104..112].copy_from_slice(&8_u64.to_le_bytes());
        bytes[288..296].copy_from_slice(&8_u64.to_le_bytes());
        assert_x86_lsl_needs_vm_review_and_blocks_uml(&bytes, 0x1005);
    }

    #[test]
    fn x86_data_sections_with_lsl_shaped_boundary_are_reviewable_in_vm() {
        let mut bytes = elf(&[0x0f, 0x03, 0x00, 0xc3], 5, 6);
        bytes[264..272].copy_from_slice(&u64::from(object::elf::SHF_ALLOC).to_le_bytes());
        bytes[288..296].copy_from_slice(&1_u64.to_le_bytes());
        assert_x86_lsl_needs_vm_review_and_blocks_uml(&bytes, 0x1000);
    }

    #[test]
    fn x86_section_decode_continues_an_instruction_into_the_next_range() {
        let mut bytes = elf(&[0xb0, 0x05, 0x0f, 0xc7, 0xf0, 0xc3], 5, 6);
        bytes[288..296].copy_from_slice(&1_u64.to_le_bytes());
        let sites = scan_elf(&bytes, "").unwrap().unwrap();
        assert_eq!(
            sites
                .iter()
                .map(|site| (site.address, site.instruction.as_str()))
                .collect::<Vec<_>>(),
            [(0x1002, "RDRAND")]
        );
    }

    #[test]
    fn x86_host_input_split_across_adjacent_executable_loads_is_reviewable() {
        for (mnemonic, opcode, modrm) in [
            ("RDRAND", 0xc7, 0xf0),
            ("RDSEED", 0xc7, 0xf8),
            ("LSL", 0x03, 0xc1),
        ] {
            for left_file_size in [4096_u64, 4094] {
                let mut bytes = vec![0; 8203];
                bytes[..16]
                    .copy_from_slice(b"\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00");
                for (offset, value) in [
                    (16, 2_u16),
                    (18, 62),
                    (52, 64),
                    (54, 56),
                    (56, 3),
                    (58, 64),
                    (60, 4),
                    (62, 3),
                ] {
                    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
                }
                bytes[20..24].copy_from_slice(&1_u32.to_le_bytes());
                for (offset, value) in [(24, 0x1ffe_u64), (32, 64), (40, 256)] {
                    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
                }
                for (index, size) in [(0_usize, left_file_size), (1, 10)] {
                    let offset = 64 + index * 56;
                    bytes[offset..offset + 4].copy_from_slice(&object::elf::PT_LOAD.to_le_bytes());
                    bytes[offset + 4..offset + 8].copy_from_slice(&5_u32.to_le_bytes());
                    for (field, value) in [
                        (8, 4096_u64 + index as u64 * 4096),
                        (16, 0x1000_u64 + index as u64 * 4096),
                        (24, 0x1000_u64 + index as u64 * 4096),
                        (32, size),
                        (40, size),
                        (48, 4096),
                    ] {
                        bytes[offset + field..offset + field + 8]
                            .copy_from_slice(&value.to_le_bytes());
                    }
                    let section = 320 + index * 64;
                    bytes[section + 4..section + 8]
                        .copy_from_slice(&object::elf::SHT_PROGBITS.to_le_bytes());
                    for (field, value) in [
                        (8, 6_u64),
                        (16, 0x1000_u64 + index as u64 * 4096),
                        (24, 4096_u64 + index as u64 * 4096),
                        (32, size),
                        (48, 1),
                    ] {
                        bytes[section + field..section + field + 8]
                            .copy_from_slice(&value.to_le_bytes());
                    }
                }
                bytes[176..180].copy_from_slice(&object::elf::PT_GNU_STACK.to_le_bytes());
                bytes[180..184].copy_from_slice(&6_u32.to_le_bytes());
                bytes[452..456].copy_from_slice(&object::elf::SHT_STRTAB.to_le_bytes());
                bytes[472..480].copy_from_slice(&8202_u64.to_le_bytes());
                bytes[480..488].copy_from_slice(&1_u64.to_le_bytes());
                bytes[496..504].copy_from_slice(&1_u64.to_le_bytes());
                bytes[4096..8192].fill(0x90);
                bytes[8190..8192].copy_from_slice(&[0x0f, opcode]);
                bytes[8192..8202]
                    .copy_from_slice(&[modrm, 0xb8, 0x3c, 0, 0, 0, 0x31, 0xff, 0x0f, 0x05]);
                let sites = scan_elf(&bytes, "").unwrap().unwrap();
                assert_eq!(
                    sites
                        .iter()
                        .map(|site| (site.address, site.instruction.as_str()))
                        .collect::<Vec<_>>(),
                    [(0x1ffe, mnemonic)],
                    "{mnemonic} with left file size {left_file_size}"
                );
                if left_file_size == 4096 {
                    bytes[8190..8192].copy_from_slice(&[0x90, 0xb0]);
                    bytes[8192..8202].copy_from_slice(&[
                        0x05, 0x0f, opcode, modrm, 0xc3, 0x90, 0x90, 0x90, 0x90, 0x90,
                    ]);
                    let sites = scan_elf(&bytes, "").unwrap().unwrap();
                    assert_eq!(
                        sites
                            .iter()
                            .map(|site| (site.address, site.instruction.as_str()))
                            .collect::<Vec<_>>(),
                        [(0x2001, mnemonic)]
                    );
                }
            }
        }
    }

    #[test]
    fn x86_memory_lsl_split_after_modrm_or_sib_across_loads_needs_review() {
        for (left, right, address) in [
            (
                vec![0x6a, 0x7b, 0xeb, 0x01, 0xb0, 0x0f, 0x03, 0x04],
                vec![0x24, 0x89, 0xc7, 0xb8, 0x3c, 0, 0, 0, 0x0f, 0x05],
                0x1ffd,
            ),
            (
                vec![0x6a, 0x7b, 0xeb, 0x01, 0xb0, 0x0f, 0x03, 0x44, 0x24],
                vec![0x08, 0x89, 0xc7, 0xb8, 0x3c, 0, 0, 0, 0x0f, 0x05],
                0x1ffc,
            ),
        ] {
            let bytes = adjacent_x86_elf(&left, &right);
            assert_x86_lsl_needs_vm_review_and_blocks_uml(&bytes, address);
        }
    }

    #[test]
    fn overlapping_executable_loads_cannot_hide_a_cross_page_instruction() {
        let mut bytes = vec![0; 0x3011];
        bytes[..16].copy_from_slice(b"\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00");
        for (offset, value) in [
            (16, 2_u16),
            (18, 62),
            (52, 64),
            (54, 56),
            (56, 2),
            (58, 64),
            (60, 4),
        ] {
            bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        }
        bytes[20..24].copy_from_slice(&1_u32.to_le_bytes());
        bytes[62..64].copy_from_slice(&3_u16.to_le_bytes());
        for (offset, value) in [(24, 0x1ffe_u64), (32, 64), (40, 256)] {
            bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        for (index, (file_offset, address, size)) in
            [(0x1000_u64, 0x1000_u64, 0x1002_u64), (0x3000, 0x2000, 13)]
                .into_iter()
                .enumerate()
        {
            let program = 64 + index * 56;
            bytes[program..program + 4].copy_from_slice(&object::elf::PT_LOAD.to_le_bytes());
            bytes[program + 4..program + 8].copy_from_slice(&5_u32.to_le_bytes());
            for (field, value) in [
                (8, file_offset),
                (16, address),
                (24, address),
                (32, size),
                (40, size),
                (48, 4096),
            ] {
                bytes[program + field..program + field + 8].copy_from_slice(&value.to_le_bytes());
            }
            let section = 320 + index * 64;
            bytes[section + 4..section + 8]
                .copy_from_slice(&object::elf::SHT_PROGBITS.to_le_bytes());
            for (field, value) in [
                (8, 6_u64),
                (16, address),
                (24, file_offset),
                (32, size),
                (48, 1),
            ] {
                bytes[section + field..section + field + 8].copy_from_slice(&value.to_le_bytes());
            }
        }
        bytes[352..360].copy_from_slice(&0x1000_u64.to_le_bytes());
        bytes[400..408].copy_from_slice(&0x2002_u64.to_le_bytes());
        bytes[408..416].copy_from_slice(&0x3002_u64.to_le_bytes());
        bytes[416..424].copy_from_slice(&11_u64.to_le_bytes());
        bytes[452..456].copy_from_slice(&object::elf::SHT_STRTAB.to_le_bytes());
        bytes[472..480].copy_from_slice(&0x3010_u64.to_le_bytes());
        bytes[480..488].copy_from_slice(&1_u64.to_le_bytes());
        bytes[496..504].copy_from_slice(&1_u64.to_le_bytes());
        bytes[0x1000..0x1ffe].fill(0x90);
        bytes[0x1ffe..0x2002].copy_from_slice(&[0x0f, 0xc7, 0x90, 0x90]);
        bytes[0x3000..0x300d]
            .copy_from_slice(&[0xf0, 0xbf, 0x25, 0, 0, 0, 0xb8, 0x3c, 0, 0, 0, 0x0f, 0x05]);
        let error = scan_elf(&bytes, "").unwrap_err().to_string();
        assert!(error.contains("overlapping executable ELF mappings"));
    }

    #[test]
    fn x86_segment_start_remains_a_decode_anchor_inside_the_first_page() {
        let mut bytes = elf(&[0x0f, 0xc7, 0xf0, 0xc3], 5, 6);
        bytes.resize(4102, 0);
        bytes[24..32].copy_from_slice(&0x1001_u64.to_le_bytes());
        bytes[72..80].copy_from_slice(&4097_u64.to_le_bytes());
        bytes[80..88].copy_from_slice(&0x1001_u64.to_le_bytes());
        bytes[272..280].copy_from_slice(&0x1004_u64.to_le_bytes());
        bytes[280..288].copy_from_slice(&4100_u64.to_le_bytes());
        bytes[288..296].copy_from_slice(&1_u64.to_le_bytes());
        bytes[344..352].copy_from_slice(&4101_u64.to_le_bytes());
        bytes[4096..4101].copy_from_slice(&[0xb8, 0x0f, 0xc7, 0xf0, 0xc3]);
        let sites = scan_elf(&bytes, "").unwrap().unwrap();
        assert_eq!(sites[0].address, 0x1001);
        assert_eq!(sites[0].instruction, "RDRAND");
    }

    #[test]
    fn arm_noncode_allocated_sections_in_executable_load_fail_even_if_unaligned() {
        let code: Vec<u8> = [0xd53b2400_u32, 0xd65f03c0]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        let mut bytes = arm_elf(&code, code.len());
        bytes[264..272].copy_from_slice(&2_u64.to_le_bytes());
        bytes[288..296].copy_from_slice(&3_u64.to_le_bytes());
        let error = scan_elf(&bytes, "").unwrap_err().to_string();
        assert!(error.contains("allocated non-executable section"));
    }

    #[test]
    fn arm_executable_load_can_end_with_partial_data_word() {
        let mut code: Vec<u8> = [0xd53b2400_u32, 0xd65f03c0]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        code.push(0xff);
        let bytes = arm_elf(&code, code.len());
        let sites = scan_elf(&bytes, "").unwrap().unwrap();
        assert_eq!(sites[0].instruction, "RNDR");
    }

    #[test]
    fn arm_executable_page_tail_can_complete_an_entropy_instruction() {
        let mut bytes = elf(&0xd53b2400_u32.to_le_bytes(), 5, 6);
        bytes[18..20].copy_from_slice(&object::elf::EM_AARCH64.to_le_bytes());
        for offset in [96, 104, 288] {
            bytes[offset..offset + 8].copy_from_slice(&2_u64.to_le_bytes());
        }
        let sites = scan_elf(&bytes, "").unwrap().unwrap();
        assert_eq!(sites[0].address, 0x1000);
        assert_eq!(sites[0].instruction, "RNDR");
    }

    #[test]
    fn executable_page_tail_with_larger_memory_size_contains_file_bytes() {
        let mut bytes = elf(&[0x90, 0x0f, 0xc7, 0xf0, 0xc3], 5, 6);
        bytes[96..104].copy_from_slice(&1_u64.to_le_bytes());
        bytes[104..112].copy_from_slice(&5_u64.to_le_bytes());
        bytes[288..296].copy_from_slice(&1_u64.to_le_bytes());
        let sites = scan_elf(&bytes, "").unwrap().unwrap();
        assert_eq!(sites[0].address, 0x1001);
        assert_eq!(sites[0].instruction, "RDRAND");
    }

    #[test]
    fn executable_page_head_before_segment_address_contains_file_bytes() {
        let mut bytes = elf(&[0xc3], 5, 6);
        bytes.resize(0x1081, 0);
        bytes[24..32].copy_from_slice(&0x1000_u64.to_le_bytes());
        bytes[72..80].copy_from_slice(&0x1080_u64.to_le_bytes());
        bytes[80..88].copy_from_slice(&0x1080_u64.to_le_bytes());
        bytes[112..120].copy_from_slice(&4096_u64.to_le_bytes());
        bytes[272..280].copy_from_slice(&0x1080_u64.to_le_bytes());
        bytes[280..288].copy_from_slice(&0x1080_u64.to_le_bytes());
        bytes[0x1000..0x1004].copy_from_slice(&[0x0f, 0xc7, 0xf0, 0xc3]);
        bytes[0x1080] = 0xc3;
        let sites = scan_elf(&bytes, "").unwrap().unwrap();
        assert_eq!(sites[0].address, 0x1000);
        assert_eq!(sites[0].instruction, "RDRAND");
    }

    #[test]
    fn arm_executable_page_head_before_segment_address_contains_file_bytes() {
        let mut bytes = elf(&0xd65f03c0_u32.to_le_bytes(), 5, 6);
        bytes.resize(0x1084, 0);
        bytes[18..20].copy_from_slice(&object::elf::EM_AARCH64.to_le_bytes());
        bytes[24..32].copy_from_slice(&0x1000_u64.to_le_bytes());
        bytes[72..80].copy_from_slice(&0x1080_u64.to_le_bytes());
        bytes[80..88].copy_from_slice(&0x1080_u64.to_le_bytes());
        bytes[272..280].copy_from_slice(&0x1080_u64.to_le_bytes());
        bytes[280..288].copy_from_slice(&0x1080_u64.to_le_bytes());
        bytes[0x1000..0x1004].copy_from_slice(&0xd53b2400_u32.to_le_bytes());
        bytes[0x1080..0x1084].copy_from_slice(&0xd65f03c0_u32.to_le_bytes());
        let sites = scan_elf(&bytes, "").unwrap().unwrap();
        assert_eq!(sites[0].address, 0x1000);
        assert_eq!(sites[0].instruction, "RNDR");
    }

    #[test]
    fn executable_load_offset_must_match_its_mapped_page_position() {
        let mut bytes = elf(&[0xc3, 0x0f, 0xc7, 0xf0], 5, 6);
        bytes[72..80].copy_from_slice(&4097_u64.to_le_bytes());
        assert!(
            scan_elf(&bytes, "")
                .unwrap_err()
                .to_string()
                .contains("page offset mismatch")
        );
    }

    #[test]
    fn relocatable_objects_are_ignored_but_loaded_bytes_are_scanned() {
        let mut relocatable = elf(&[0x0f, 0xc7, 0xf0], 5, 6);
        relocatable[16..18].copy_from_slice(&object::elf::ET_REL.to_le_bytes());
        assert!(scan_elf(&relocatable, "").unwrap().is_none());
        let mut debug = elf(&[0x0f, 0xc7, 0xf0], 5, 6);
        debug[260..264].copy_from_slice(&object::elf::SHT_NOBITS.to_le_bytes());
        assert_eq!(
            scan_elf(&debug, "").unwrap().unwrap()[0].instruction,
            "RDRAND"
        );
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
