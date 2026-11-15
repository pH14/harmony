// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    fs,
    io::{self, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use sha2::{Digest, Sha256};

use super::SessionConfig;

const MAGIC: [u8; 8] = *b"HMBOOT01";
const KEY_DOMAIN: &[u8] = b"consonance-boot-cache-key-v1\0";
const PAGE_SIZE: usize = 4096;
const KEPT_ENTRIES: usize = 4;

pub(super) struct Slot {
    dir: PathBuf,
    key: [u8; 32],
}

pub(super) struct Entry {
    pub(super) at: u64,
    pub(super) pages: Vec<(u64, Arc<[u8; PAGE_SIZE]>)>,
    pub(super) sidecar: Vec<u8>,
}

impl Slot {
    pub(super) fn new(
        dir: &Path,
        image_identity: &[u8; 32],
        config: &SessionConfig,
    ) -> Option<Self> {
        Some(Self {
            dir: dir.to_path_buf(),
            key: key(image_identity, config, &host_identity()?),
        })
    }

    fn path(&self) -> PathBuf {
        self.dir
            .join(format!("{}.boot", super::bytes_hex(&self.key)))
    }

    pub(super) fn load(&self) -> Option<Entry> {
        let file = fs::File::open(self.path()).ok()?;
        read_entry(&mut BufReader::with_capacity(1 << 20, file), &self.key).ok()
    }

    pub(super) fn store(&self, at: u64, pages: &[(u64, Arc<[u8; PAGE_SIZE]>)], sidecar: &[u8]) {
        let _ = self.store_inner(at, pages, sidecar);
    }

    fn store_inner(
        &self,
        at: u64,
        pages: &[(u64, Arc<[u8; PAGE_SIZE]>)],
        sidecar: &[u8],
    ) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let path = self.path();
        let partial = path.with_extension(format!("partial-{}", std::process::id()));
        let written = (|| {
            let mut out = BufWriter::with_capacity(1 << 20, fs::File::create(&partial)?);
            write_entry(&mut out, &self.key, at, pages, sidecar)?;
            out.into_inner()
                .map_err(io::IntoInnerError::into_error)?
                .sync_all()
        })();
        if let Err(error) = written.and_then(|()| fs::rename(&partial, &path)) {
            let _ = fs::remove_file(&partial);
            return Err(error);
        }
        self.evict(&path);
        Ok(())
    }

    fn evict(&self, keep: &Path) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        let mut found: Vec<(SystemTime, PathBuf)> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path != keep && path.extension().is_some_and(|ext| ext == "boot"))
            .filter_map(|path| Some((fs::metadata(&path).ok()?.modified().ok()?, path)))
            .collect();
        found.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        for (_, path) in found.into_iter().skip(KEPT_ENTRIES - 1) {
            let _ = fs::remove_file(path);
        }
    }
}

fn key(image_identity: &[u8; 32], config: &SessionConfig, host: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(KEY_DOMAIN);
    digest.update(image_identity);
    digest.update([u8::from(config.defer_virtual_time_checkpoint_hashes)]);
    digest.update((host.len() as u64).to_le_bytes());
    digest.update(host);
    digest.finalize().into()
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn host_identity() -> Option<Vec<u8>> {
    let allowed = crate::placement::allowed_cpus().ok()?;
    let cpuinfo = fs::read_to_string("/proc/cpuinfo").ok()?;
    let mut out = host_cpus(&cpuinfo, &allowed)?;
    for path in ["/proc/sys/kernel/osrelease", "/proc/sys/kernel/version"] {
        let text = fs::read(path).ok()?;
        out.extend_from_slice(&(text.len() as u64).to_le_bytes());
        out.extend_from_slice(&text);
    }
    Some(out)
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn host_identity() -> Option<Vec<u8>> {
    None
}

#[cfg_attr(
    not(all(target_os = "linux", target_arch = "x86_64")),
    allow(dead_code)
)]
fn host_cpus(cpuinfo: &str, allowed: &std::collections::BTreeSet<usize>) -> Option<Vec<u8>> {
    const FIELDS: [&str; 6] = [
        "vendor_id",
        "cpu family",
        "model",
        "stepping",
        "microcode",
        "flags",
    ];
    let mut cores = std::collections::BTreeSet::new();
    for block in cpuinfo.split("\n\n") {
        let mut processor = None;
        let mut fields = String::new();
        for line in block.lines() {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let (name, value) = (name.trim(), value.trim());
            if name == "processor" {
                processor = value.parse::<usize>().ok();
            } else if FIELDS.contains(&name) {
                fields.push_str(name);
                fields.push('=');
                fields.push_str(value);
                fields.push('\n');
            }
        }
        if processor.is_some_and(|cpu| allowed.contains(&cpu)) {
            cores.insert(fields);
        }
    }
    if cores.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for core in cores {
        out.extend_from_slice(&(core.len() as u64).to_le_bytes());
        out.extend_from_slice(core.as_bytes());
    }
    Some(out)
}

fn write_entry(
    out: &mut impl Write,
    key: &[u8; 32],
    at: u64,
    pages: &[(u64, Arc<[u8; PAGE_SIZE]>)],
    sidecar: &[u8],
) -> io::Result<()> {
    let mut digest = Sha256::new();
    let header = header(key, at, pages.len() as u64, sidecar.len() as u64);
    digest.update(header);
    digest.update(sidecar);
    out.write_all(&header)?;
    out.write_all(sidecar)?;
    for (gfn, page) in pages {
        out.write_all(&gfn.to_le_bytes())?;
        out.write_all(&page[..])?;
    }
    out.write_all(&digest.finalize())
}

fn header(key: &[u8; 32], at: u64, pages: u64, sidecar: u64) -> [u8; 64] {
    let mut header = [0; 64];
    header[..8].copy_from_slice(&MAGIC);
    header[8..40].copy_from_slice(key);
    header[40..48].copy_from_slice(&at.to_le_bytes());
    header[48..56].copy_from_slice(&pages.to_le_bytes());
    header[56..64].copy_from_slice(&sidecar.to_le_bytes());
    header
}

fn read_entry(input: &mut impl Read, key: &[u8; 32]) -> io::Result<Entry> {
    let invalid = |reason: &str| io::Error::new(io::ErrorKind::InvalidData, reason.to_owned());
    let mut header = [0; 64];
    input.read_exact(&mut header)?;
    if header[..8] != MAGIC || header[8..40] != key[..] {
        return Err(invalid("boot cache entry identity differs"));
    }
    let word = |range: std::ops::Range<usize>| {
        u64::from_le_bytes(header[range].try_into().expect("eight header bytes"))
    };
    let at = word(40..48);
    let count = usize::try_from(word(48..56)).map_err(|_| invalid("page count"))?;
    let sidecar_len = usize::try_from(word(56..64)).map_err(|_| invalid("sidecar length"))?;
    let mut sidecar = Vec::new();
    sidecar
        .try_reserve_exact(sidecar_len)
        .map_err(|_| invalid("sidecar length"))?;
    input.take(sidecar_len as u64).read_to_end(&mut sidecar)?;
    if sidecar.len() != sidecar_len {
        return Err(invalid("truncated sidecar"));
    }
    let mut digest = Sha256::new();
    digest.update(header);
    digest.update(&sidecar);
    let mut pages = Vec::new();
    pages
        .try_reserve_exact(count)
        .map_err(|_| invalid("page count"))?;
    for _ in 0..count {
        let mut gfn = [0; 8];
        input.read_exact(&mut gfn)?;
        let mut page = Arc::new([0; PAGE_SIZE]);
        input.read_exact(&mut Arc::get_mut(&mut page).expect("new page is unique")[..])?;
        pages.push((u64::from_le_bytes(gfn), page));
    }
    let mut trailer = [0; 32];
    input.read_exact(&mut trailer)?;
    if trailer[..] != digest.finalize()[..] {
        return Err(invalid("boot cache entry digest differs"));
    }
    if input.read(&mut [0])? != 0 {
        return Err(invalid("trailing bytes"));
    }
    Ok(Entry { at, pages, sidecar })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pages() -> Vec<(u64, Arc<[u8; PAGE_SIZE]>)> {
        vec![(3, Arc::new([7; PAGE_SIZE])), (9, Arc::new([1; PAGE_SIZE]))]
    }

    #[test]
    fn entries_round_trip_and_reject_other_keys_and_damage() {
        let mut bytes = Vec::new();
        write_entry(&mut bytes, &[5; 32], 77, &pages(), b"sidecar").unwrap();
        let entry = read_entry(&mut bytes.as_slice(), &[5; 32]).unwrap();
        assert_eq!(entry.at, 77);
        assert_eq!(entry.sidecar, b"sidecar");
        assert_eq!(entry.pages, pages());
        assert!(read_entry(&mut bytes.as_slice(), &[6; 32]).is_err());
        let mut damaged = bytes.clone();
        damaged[64] ^= 1;
        assert!(read_entry(&mut damaged.as_slice(), &[5; 32]).is_err());
        let mut truncated = bytes.clone();
        truncated.pop();
        assert!(read_entry(&mut truncated.as_slice(), &[5; 32]).is_err());
        bytes.push(0);
        assert!(read_entry(&mut bytes.as_slice(), &[5; 32]).is_err());
    }

    #[test]
    fn keys_cover_image_deferral_and_host() {
        let config = SessionConfig::default();
        let base = key(&[1; 32], &config, b"host");
        assert_ne!(base, key(&[2; 32], &config, b"host"));
        assert_ne!(base, key(&[1; 32], &config, b"other host"));
        assert_ne!(
            base,
            key(
                &[1; 32],
                &config
                    .clone()
                    .with_deferred_virtual_time_checkpoint_hashes(),
                b"host"
            )
        );
        assert_eq!(
            base,
            key(
                &[1; 32],
                &config.with_wall_limit(std::time::Duration::from_secs(1)),
                b"host"
            )
        );
    }

    #[test]
    fn host_cpus_cover_only_allowed_processors() {
        let cpuinfo = "processor\t: 0\nvendor_id\t: GenuineIntel\nmodel\t\t: 183\nmicrocode\t: 0x12b\ncpu MHz\t\t: 800\n\nprocessor\t: 1\nvendor_id\t: GenuineIntel\nmodel\t\t: 190\nmicrocode\t: 0x12b\ncpu MHz\t\t: 3000\n";
        let first = host_cpus(cpuinfo, &[0].into()).unwrap();
        let second = host_cpus(cpuinfo, &[1].into()).unwrap();
        assert_ne!(first, second);
        assert_eq!(
            first,
            host_cpus(&cpuinfo.replace("800", "900"), &[0].into()).unwrap()
        );
        assert!(host_cpus(cpuinfo, &[4].into()).is_none());
    }

    #[test]
    fn stores_replace_atomically_and_keep_the_newest_entries() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("boot");
        for index in 0..6_u8 {
            let slot = Slot {
                dir: dir.clone(),
                key: [index; 32],
            };
            slot.store_inner(u64::from(index), &pages(), b"s").unwrap();
            assert_eq!(slot.load().unwrap().at, u64::from(index));
        }
        let names: Vec<_> = fs::read_dir(&dir).unwrap().filter_map(Result::ok).collect();
        assert_eq!(names.len(), KEPT_ENTRIES);
    }
}
