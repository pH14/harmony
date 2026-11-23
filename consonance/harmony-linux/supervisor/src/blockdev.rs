// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::JoinHandle;

use rustix::fs::{CWD, FileType, Mode, makedev, mknodat};
use rustix::mount::{MountFlags, MountPropagationFlags, UnmountFlags};

use crate::bundle::DiskSpec;
use crate::disk::{
    self, DRAIN_MARK, DiskMode, FORMAT_MARK, Flip, Folded, LogEntry, LogError, Memory, Plan,
    RAM_DEVICE_BYTES, RAM_DEVICES_PER_DISK, SECTOR_BYTES, Sectors, dm,
};

const RAM_MAJOR: u32 = 1;
const MAPPER_CONTROL: &str = "/dev/mapper/control";
const MISC_MAJOR: u32 = 10;
const MAPPER_MINOR: u32 = 236;
const CHUNK_BYTES: usize = 1 << 20;
const CHUNK_SECTORS: u64 = (CHUNK_BYTES / SECTOR_BYTES) as u64;
const BLKDISCARD: u64 = 0x1277;
const MKFS: [&str; 7] = ["mkfs.ext4", "-F", "-q", "-b", "4096", "-m", "0"];
const MKFS_EXTENDED: &str = "lazy_itable_init=1,lazy_journal_init=1,nodiscard";
const MOUNT_DATA: &std::ffi::CStr = c"noinit_itable";
const LOG_SECTORS: u64 = RAM_DEVICE_BYTES / SECTOR_BYTES as u64;
pub const COMPACT_LOG_BYTES: u64 = 64 << 20;
pub const COMPACT_POLL_TICKS: u64 = 100;
const DRAIN_POLLS: u32 = 1000;
const DRAIN_WAIT: std::time::Duration = std::time::Duration::from_millis(1);

#[repr(C, align(4096))]
struct Chunk([u8; CHUNK_BYTES]);

pub struct RamDevice {
    file: File,
    chunk: Box<Chunk>,
    path: PathBuf,
}

impl RamDevice {
    fn open(index: u32) -> io::Result<Self> {
        Self::opened(index, true)
    }

    fn reader(index: u32) -> io::Result<Self> {
        Self::opened(index, false)
    }

    fn opened(index: u32, write: bool) -> io::Result<Self> {
        let path = ram_path(index);
        if !path.exists() {
            mknodat(
                CWD,
                &path,
                FileType::BlockDevice,
                Mode::from_raw_mode(0o600),
                makedev(RAM_MAJOR, index),
            )?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(write)
            .custom_flags(libc::O_DIRECT)
            .open(&path)?;
        Ok(Self {
            file,
            chunk: Box::new(Chunk([0; CHUNK_BYTES])),
            path,
        })
    }

    fn bytes(&mut self) -> io::Result<u64> {
        self.file.seek(SeekFrom::End(0))
    }

    fn discard(&self, start: u64, bytes: u64) -> io::Result<()> {
        let range = [start, bytes];
        match ioctl(&self.file, BLKDISCARD, range.as_ptr().cast_mut().cast()) {
            Err(error) if error.raw_os_error() == Some(libc::EOPNOTSUPP) => Ok(()),
            result => result,
        }
    }

    fn context(&self, error: io::Error) -> String {
        format!("{}: {error}", self.path.display())
    }
}

impl Sectors for RamDevice {
    fn read_at(&mut self, sector: u64, buffer: &mut [u8]) -> io::Result<()> {
        let mut done = 0;
        while done < buffer.len() {
            let count = (buffer.len() - done).min(CHUNK_BYTES);
            let offset = sector * SECTOR_BYTES as u64 + done as u64;
            self.file
                .read_exact_at(&mut self.chunk.0[..count], offset)?;
            buffer[done..done + count].copy_from_slice(&self.chunk.0[..count]);
            done += count;
        }
        Ok(())
    }

    fn write_at(&mut self, sector: u64, buffer: &[u8]) -> io::Result<()> {
        let mut done = 0;
        while done < buffer.len() {
            let count = (buffer.len() - done).min(CHUNK_BYTES);
            let offset = sector * SECTOR_BYTES as u64 + done as u64;
            self.chunk.0[..count].copy_from_slice(&buffer[done..done + count]);
            self.file.write_all_at(&self.chunk.0[..count], offset)?;
            done += count;
        }
        Ok(())
    }
}

struct Cached<'a> {
    device: &'a mut RamDevice,
    start: u64,
    sectors: u64,
}

impl<'a> Cached<'a> {
    fn new(device: &'a mut RamDevice) -> Self {
        Self {
            device,
            start: 0,
            sectors: 0,
        }
    }
}

impl Sectors for Cached<'_> {
    fn read_at(&mut self, sector: u64, buffer: &mut [u8]) -> io::Result<()> {
        let wanted = (buffer.len() / SECTOR_BYTES) as u64;
        if sector < self.start || sector.saturating_add(wanted) > self.start + self.sectors {
            let sectors = CHUNK_SECTORS.min(LOG_SECTORS.saturating_sub(sector));
            if wanted > sectors {
                self.sectors = 0;
                return self.device.read_at(sector, buffer);
            }
            let bytes = usize::try_from(sectors).unwrap_or(0) * SECTOR_BYTES;
            self.device.file.read_exact_at(
                &mut self.device.chunk.0[..bytes],
                sector * SECTOR_BYTES as u64,
            )?;
            self.start = sector;
            self.sectors = sectors;
        }
        let at = usize::try_from(sector - self.start).unwrap_or(0) * SECTOR_BYTES;
        buffer.copy_from_slice(&self.device.chunk.0[at..at + buffer.len()]);
        Ok(())
    }

    fn write_at(&mut self, _: u64, _: &[u8]) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

struct Growing<'a> {
    device: &'a mut RamDevice,
    image: Memory,
}

impl Sectors for Growing<'_> {
    fn read_at(&mut self, sector: u64, buffer: &mut [u8]) -> io::Result<()> {
        let end = usize::try_from(sector)
            .ok()
            .and_then(|sector| sector.checked_mul(SECTOR_BYTES))
            .and_then(|at| at.checked_add(buffer.len()))
            .filter(|&end| end as u64 <= RAM_DEVICE_BYTES)
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        while self.image.0.len() < end {
            let start = self.image.0.len();
            let bytes = CHUNK_BYTES.min(RAM_DEVICE_BYTES as usize - start);
            self.image
                .0
                .try_reserve(bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::OutOfMemory, error))?;
            self.image.0.resize(start + bytes, 0);
            self.device
                .read_at((start / SECTOR_BYTES) as u64, &mut self.image.0[start..])?;
            self.device.discard(start as u64, bytes as u64)?;
        }
        self.image.read_at(sector, buffer)
    }

    fn write_at(&mut self, _: u64, _: &[u8]) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

fn ram_path(index: u32) -> PathBuf {
    PathBuf::from(format!("/dev/ram{index}"))
}

fn ioctl(file: &File, request: u64, argument: *mut libc::c_void) -> io::Result<()> {
    // SAFETY: `file` stays open for the call, and every caller passes a
    // request whose argument layout matches `argument`: a two-word range for
    // the block discard request, which the kernel only reads, so a pointer
    // made from a shared borrow is sound, or a device-mapper buffer at least
    // as long as the `data_size` it declares.
    let result = unsafe { libc::ioctl(file.as_raw_fd(), request as _, argument) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

struct Mapper {
    control: File,
}

struct Mapped {
    path: PathBuf,
    device: String,
}

impl Mapper {
    fn open() -> io::Result<Self> {
        std::fs::create_dir_all("/dev/mapper")?;
        if !Path::new(MAPPER_CONTROL).exists() {
            mknodat(
                CWD,
                MAPPER_CONTROL,
                FileType::CharacterDevice,
                Mode::from_raw_mode(0o600),
                makedev(MISC_MAJOR, MAPPER_MINOR),
            )?;
        }
        let control = OpenOptions::new()
            .read(true)
            .write(true)
            .open(MAPPER_CONTROL)?;
        Ok(Self { control })
    }

    fn call(&self, command: u8, mut buffer: Vec<u8>, what: &str) -> Result<Vec<u8>, String> {
        ioctl(
            &self.control,
            dm::request(command),
            buffer.as_mut_ptr().cast(),
        )
        .map_err(|error| format!("device-mapper {what}: {error}"))?;
        Ok(buffer)
    }

    fn create(
        &self,
        name: &str,
        sectors: u64,
        target: &str,
        params: &str,
    ) -> Result<Mapped, String> {
        let reply = self.call(
            dm::DEV_CREATE,
            dm::header(name, 0)?,
            &format!("create {name}"),
        )?;
        let (major, minor) = dm::device(&reply);
        self.load(name, sectors, target, params)?;
        self.resume(name)?;
        let path = PathBuf::from(format!("/dev/mapper/{name}"));
        mknodat(
            CWD,
            &path,
            FileType::BlockDevice,
            Mode::from_raw_mode(0o600),
            makedev(major, minor),
        )
        .map_err(|error| format!("{}: {error}", path.display()))?;
        Ok(Mapped {
            path,
            device: format!("{major}:{minor}"),
        })
    }

    fn load(&self, name: &str, sectors: u64, target: &str, params: &str) -> Result<(), String> {
        self.call(
            dm::TABLE_LOAD,
            dm::table(name, sectors, target, params)?,
            &format!("load {target} table {name}"),
        )?;
        Ok(())
    }

    fn suspend(&self, name: &str) -> Result<(), String> {
        self.call(
            dm::DEV_SUSPEND,
            dm::header(name, dm::SUSPEND | dm::NOFLUSH | dm::SKIP_LOCKFS)?,
            &format!("suspend {name}"),
        )?;
        Ok(())
    }

    fn resume(&self, name: &str) -> Result<(), String> {
        self.call(
            dm::DEV_SUSPEND,
            dm::header(name, 0)?,
            &format!("resume {name}"),
        )?;
        Ok(())
    }

    fn replace(&self, name: &str, sectors: u64, target: &str, params: &str) -> Result<(), String> {
        self.suspend(name)?;
        self.load(name, sectors, target, params)?;
        self.resume(name)
    }

    fn fail(&self, name: &str, sectors: u64) -> Result<(), String> {
        self.replace(name, sectors, "error", "")
    }

    fn status(&self, name: &str) -> Result<String, String> {
        let reply = self.call(
            dm::TABLE_STATUS,
            dm::status(name)?,
            &format!("status {name}"),
        )?;
        dm::status_line(&reply)
            .map(str::to_owned)
            .map_err(|error| format!("{name}: {error}"))
    }

    fn send(&self, name: &str, text: &str) -> Result<(), String> {
        self.call(
            dm::TARGET_MSG,
            dm::message(name, text)?,
            &format!("message {name} {text:?}"),
        )?;
        Ok(())
    }

    fn remove(&self, name: &str) -> Result<(), String> {
        self.call(
            dm::DEV_REMOVE,
            dm::header(name, dm::DEFERRED_REMOVE)?,
            &format!("remove {name}"),
        )?;
        let _ = std::fs::remove_file(format!("/dev/mapper/{name}"));
        Ok(())
    }
}

pub struct Corruption {
    pub flip: Option<Flip>,
    pub unmountable: Option<String>,
}

impl Corruption {
    #[must_use]
    pub fn describe(&self) -> String {
        let mut text = self.flip.map_or_else(
            || "no plain write since the last cut to corrupt".to_owned(),
            Flip::describe,
        );
        if let Some(error) = &self.unmountable {
            text.push_str(&format!(
                "; the disk did not mount ({error}), so it now holds a new empty filesystem"
            ));
        }
        text
    }
}

struct Disk {
    spec: DiskSpec,
    first: u32,
    generation: u64,
    mode: DiskMode,
    log: String,
    folded: Folded,
    folding: Option<JoinHandle<Result<Folded, String>>>,
    compacted: Vec<String>,
}

impl Disk {
    fn sectors(&self) -> u64 {
        self.spec.bytes / SECTOR_BYTES as u64
    }

    fn name(&self) -> String {
        format!("harmony-disk-{}-{}", self.spec.node, self.generation)
    }

    fn log_name(&self) -> String {
        format!("harmony-log-{}-{}", self.spec.node, self.generation)
    }

    fn table(&self) -> String {
        format!("{RAM_MAJOR}:{} {RAM_MAJOR}:{}", self.first, self.first + 1)
    }

    fn attach(&mut self, mapper: &Mapper) -> Result<PathBuf, String> {
        let log = mapper.create(
            &self.log_name(),
            self.sectors(),
            "log-writes",
            &self.table(),
        )?;
        self.log = log.device;
        let (target, params) = DiskMode::Normal.table(&self.log);
        Ok(mapper
            .create(&self.name(), self.sectors(), target, &params)?
            .path)
    }

    fn apply_mode(&self, mapper: &Mapper) -> Result<(), String> {
        let (target, params) = self.mode.table(&self.log);
        mapper.replace(&self.name(), self.sectors(), target, &params)
    }

    fn restore_mode(&self, mapper: &Mapper) -> Result<(), String> {
        if self.mode == DiskMode::Normal {
            return Ok(());
        }
        self.apply_mode(mapper)
    }

    fn mount(&self, device: &Path) -> Result<(), String> {
        let path = Path::new(&self.spec.path);
        rustix::mount::mount(device, path, "ext4", MountFlags::empty(), MOUNT_DATA).map_err(
            |error| format!("mount {} on {}: {error}", device.display(), path.display()),
        )?;
        rustix::mount::mount_change(path, MountPropagationFlags::SHARED)
            .map_err(|error| format!("share mount {}: {error}", path.display()))
    }

    fn read_only(&self) -> Result<bool, String> {
        let stat = rustix::fs::statvfs(Path::new(&self.spec.path))
            .map_err(|error| format!("statvfs {}: {error}", self.spec.path))?;
        if stat.f_flag.contains(rustix::fs::StatVfsMountFlags::RDONLY) {
            return Ok(true);
        }
        let mountinfo = std::fs::read_to_string("/proc/self/mountinfo")
            .map_err(|error| format!("/proc/self/mountinfo: {error}"))?;
        Ok(disk::emergency_read_only(&mountinfo, &self.spec.path))
    }

    fn sync_failed(&self) -> Result<bool, String> {
        let root =
            File::open(&self.spec.path).map_err(|error| format!("{}: {error}", self.spec.path))?;
        disk::journal_failed(rustix::fs::syncfs(&root).map_err(io::Error::from))
            .map_err(|error| format!("sync {}: {error}", self.spec.path))
    }

    fn sync(&self) -> Result<(), String> {
        let root =
            File::open(&self.spec.path).map_err(|error| format!("{}: {error}", self.spec.path))?;
        rustix::fs::syncfs(&root).map_err(|error| format!("sync {}: {error}", self.spec.path))
    }

    fn format(&self, mapper: &Mapper, device: &Path, uid: u32, gid: u32) -> Result<(), String> {
        let mut command = Command::new(MKFS[0]);
        command
            .args(&MKFS[1..])
            .arg("-E")
            .arg(format!("{MKFS_EXTENDED},root_owner={uid}:{gid}"))
            .arg(device)
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        crate::process::restore_oom_score(&mut command);
        let status = command.status().map_err(|error| {
            format!(
                "{}: {error}; node disks need e2fsprogs in the image",
                MKFS[0]
            )
        })?;
        if !status.success() {
            return Err(format!("{} {} exited {status}", MKFS[0], device.display()));
        }
        std::fs::create_dir_all(&self.spec.path)
            .map_err(|error| format!("{}: {error}", self.spec.path))?;
        self.mount(device)?;
        let lost = Path::new(&self.spec.path).join("lost+found");
        std::fs::remove_dir(&lost).map_err(|error| format!("{}: {error}", lost.display()))?;
        self.sync()?;
        mapper.send(&self.log_name(), &format!("mark {FORMAT_MARK}"))
    }

    fn drained(&self, mapper: &Mapper, log: &mut RamDevice) -> Result<bool, String> {
        let log_name = self.log_name();
        let line = mapper.status(&log_name)?;
        if dm::logging_disabled(&line) {
            return Ok(true);
        }
        let (Some(entries), Some(last)) = (dm::logged_entries(&line), dm::logged_sectors(&line))
        else {
            return Err(format!("{log_name}: unexpected status {line:?}"));
        };
        if !disk::drain_marked(log, last).map_err(|error| log_error(&self.spec, &error))? {
            return Ok(false);
        }
        disk::drain_logged(&mut Cached::new(log), entries, last)
            .map_err(|error| log_error(&self.spec, &error))
    }

    fn retire_log(&self, mapper: &Mapper, target: &str, params: &str) -> Result<(), String> {
        let log_name = self.log_name();
        mapper.send(&log_name, &format!("mark {DRAIN_MARK}"))?;
        let device = self.first + 1;
        let mut log = RamDevice::reader(device)
            .map_err(|error| format!("{}: {error}", ram_path(device).display()))?;
        let mut polls = 1;
        while !self.drained(mapper, &mut log)? {
            if polls == DRAIN_POLLS {
                return Err(format!(
                    "{log_name}: the write log did not log its drain mark"
                ));
            }
            polls += 1;
            std::thread::sleep(DRAIN_WAIT);
        }
        drop(log);
        mapper.load(&log_name, self.sectors(), target, params)?;
        mapper.resume(&log_name)
    }

    fn cut(&mut self, mapper: &Mapper) -> Result<Vec<LogEntry>, String> {
        let name = self.name();
        let log_name = self.log_name();
        mapper.suspend(&log_name)?;
        self.finish_fold()?;
        self.retire_log(mapper, "error", "")?;
        mapper.fail(&name, self.sectors())?;
        rustix::mount::unmount(Path::new(&self.spec.path), UnmountFlags::DETACH)
            .map_err(|error| format!("unmount {}: {error}", self.spec.path))?;
        mapper.remove(&name)?;
        mapper.remove(&log_name)?;
        let mut log = self.open(1)?;
        let logged = disk::read_log(&mut Cached::new(&mut log), LOG_SECTORS)
            .map_err(|error| log_error(&self.spec, &error))?;
        Ok(self.folded.join(&logged))
    }

    fn rebuild(&mut self, plan: &Plan, flip: Option<Flip>) -> Result<(), String> {
        let mut data = self.open(0)?;
        let mut log = self.open(1)?;
        let mut base = self.open(2)?;
        self.folded
            .rebuild(plan, &mut log, &mut base, &mut data, self.sectors())
            .map_err(|error| log_error(&self.spec, &error))?;
        if let Some(flip) = flip {
            flip.apply(&mut data).map_err(|error| data.context(error))?;
            flip.apply(&mut base).map_err(|error| base.context(error))?;
        }
        clear(&mut log)?;
        self.folded = Folded::default();
        self.generation += 1;
        Ok(())
    }

    fn log_full(&self, mapper: &Mapper) -> Result<bool, String> {
        let line = mapper.status(&self.log_name())?;
        let sectors = dm::logged_sectors(&line)
            .ok_or_else(|| format!("{}: unexpected status {line:?}", self.log_name()))?;
        Ok(sectors >= COMPACT_LOG_BYTES / SECTOR_BYTES as u64)
    }

    fn compact(&mut self, mapper: &Mapper) -> Result<(), String> {
        let name = self.name();
        let log_name = self.log_name();
        let (target, params) = DiskMode::Normal.table(&format!("{RAM_MAJOR}:{}", self.first));
        mapper.suspend(&name)?;
        self.finish_fold()?;
        mapper.suspend(&log_name)?;
        self.retire_log(mapper, target, &params)?;
        let mut log = self.open(1)?;
        let mut growing = Growing {
            device: &mut log,
            image: Memory::default(),
        };
        let logged = disk::read_log(&mut growing, LOG_SECTORS)
            .map_err(|error| log_error(&self.spec, &error))?;
        let mut image = growing.image;
        clear(&mut log)?;
        drop(log);
        mapper.replace(&log_name, self.sectors(), "log-writes", &self.table())?;
        mapper.resume(&name)?;
        let mut base = self.open(2)?;
        let mut folded = std::mem::take(&mut self.folded);
        let spec = self.spec.clone();
        let sectors = self.sectors();
        let folding = std::thread::Builder::new()
            .spawn(move || {
                folded
                    .fold(&logged, &mut image, &mut base, sectors)
                    .map_err(|error| log_error(&spec, &error))?;
                Ok(folded)
            })
            .map_err(|error| {
                format!(
                    "node {} disk {}: write log fold thread: {error}",
                    self.spec.node, self.spec.path
                )
            })?;
        self.folding = Some(folding);
        Ok(())
    }

    fn finish_fold(&mut self) -> Result<(), String> {
        let Some(folding) = self.folding.take() else {
            return Ok(());
        };
        self.folded = folding.join().map_err(|_| {
            format!(
                "node {} disk {}: the write log fold panicked",
                self.spec.node, self.spec.path
            )
        })??;
        self.compacted.push(format!(
            "node {} disk log compacted: {}",
            self.spec.node,
            self.folded.describe()
        ));
        Ok(())
    }

    fn open(&self, offset: u32) -> Result<RamDevice, String> {
        let device = self.first + offset;
        RamDevice::open(device).map_err(|error| format!("{}: {error}", ram_path(device).display()))
    }
}

pub struct Disks {
    mapper: Option<Mapper>,
    disks: Vec<Disk>,
    uid: u32,
    gid: u32,
}

impl Disks {
    pub fn create(specs: &[DiskSpec], uid: u32, gid: u32) -> Result<Self, String> {
        if specs.is_empty() {
            return Ok(Self {
                mapper: None,
                disks: Vec::new(),
                uid,
                gid,
            });
        }
        let mapper = Mapper::open().map_err(|error| format!("{MAPPER_CONTROL}: {error}"))?;
        let mut disks = Vec::with_capacity(specs.len());
        for (index, spec) in specs.iter().enumerate() {
            let first = u32::try_from(index)
                .ok()
                .and_then(|index| index.checked_mul(RAM_DEVICES_PER_DISK))
                .ok_or("too many node disks")?;
            for device in first..first + RAM_DEVICES_PER_DISK {
                let mut ram = RamDevice::open(device)
                    .map_err(|error| format!("{}: {error}", ram_path(device).display()))?;
                let bytes = ram.bytes().map_err(|error| ram.context(error))?;
                if bytes != RAM_DEVICE_BYTES {
                    return Err(format!(
                        "{} holds {bytes} bytes; node disks need {RAM_DEVICE_BYTES}-byte RAM devices",
                        ram.path.display()
                    ));
                }
            }
            let mut disk = Disk {
                spec: spec.clone(),
                first,
                generation: 0,
                mode: DiskMode::Normal,
                log: String::new(),
                folded: Folded::default(),
                folding: None,
                compacted: Vec::new(),
            };
            let device = disk.attach(&mapper)?;
            disk.format(&mapper, &device, uid, gid)?;
            disks.push(disk);
        }
        Ok(Self {
            mapper: Some(mapper),
            disks,
            uid,
            gid,
        })
    }

    fn find(&mut self, node: u16) -> Option<(&Mapper, &mut Disk)> {
        let mapper = self.mapper.as_ref()?;
        let disk = self.disks.iter_mut().find(|disk| disk.spec.node == node)?;
        Some((mapper, disk))
    }

    pub fn power_loss(&mut self, node: u16, choice: u64) -> Result<Option<Plan>, String> {
        let Some((mapper, disk)) = self.find(node) else {
            return Ok(None);
        };
        let entries = disk.cut(mapper)?;
        let plan = disk.folded.plan(&entries, disk::node_choice(choice, node));
        disk.rebuild(&plan, None)?;
        let device = disk.attach(mapper)?;
        disk.mount(&device)?;
        disk.restore_mode(mapper)?;
        Ok(Some(plan))
    }

    pub fn corrupt(&mut self, node: u16, choice: u64) -> Result<Option<Corruption>, String> {
        let (uid, gid) = (self.uid, self.gid);
        let Some((mapper, disk)) = self.find(node) else {
            return Ok(None);
        };
        if disk.mode == DiskMode::Normal {
            let _ = disk.sync();
        }
        let entries = disk.cut(mapper)?;
        let flip = disk
            .folded
            .corruption(&entries, disk::node_choice(choice, node));
        disk.rebuild(&disk.folded.plan_all(&entries), flip)?;
        let device = disk.attach(mapper)?;
        let unmountable = match disk.mount(&device) {
            Ok(()) => None,
            Err(error) => {
                disk.format(mapper, &device, uid, gid)?;
                Some(error)
            }
        };
        disk.restore_mode(mapper)?;
        Ok(Some(Corruption { flip, unmountable }))
    }

    pub fn failed(&mut self, node: u16, sync: bool) -> Result<bool, String> {
        let Some((_, disk)) = self.find(node) else {
            return Ok(false);
        };
        if sync && disk.sync_failed()? {
            return Ok(true);
        }
        disk.read_only()
    }

    pub fn remount(&mut self, node: u16) -> Result<Option<Plan>, String> {
        let Some((mapper, disk)) = self.find(node) else {
            return Ok(None);
        };
        let entries = disk.cut(mapper)?;
        let plan = disk.folded.plan_all(&entries);
        disk.rebuild(&plan, None)?;
        let device = disk.attach(mapper)?;
        disk.mount(&device)?;
        disk.restore_mode(mapper)?;
        Ok(Some(plan))
    }

    pub fn set_mode(&mut self, node: u16, mode: DiskMode) -> Result<bool, String> {
        let Some((mapper, disk)) = self.find(node) else {
            return Ok(false);
        };
        disk.mode = mode;
        disk.apply_mode(mapper)?;
        Ok(true)
    }

    pub fn compact(&mut self) -> Result<Vec<String>, String> {
        let Some(mapper) = self.mapper.as_ref() else {
            return Ok(Vec::new());
        };
        let mut compacted = Vec::new();
        for disk in &mut self.disks {
            if disk.folding.as_ref().is_some_and(JoinHandle::is_finished) {
                disk.finish_fold()?;
            }
            if disk.mode == DiskMode::Normal && disk.log_full(mapper)? {
                disk.compact(mapper)?;
            }
            compacted.append(&mut disk.compacted);
        }
        Ok(compacted)
    }

    pub fn release_stalled(&mut self, node: u16) -> Result<(), String> {
        let Some((mapper, disk)) = self.find(node) else {
            return Ok(());
        };
        if !matches!(disk.mode, DiskMode::Stall { .. }) {
            return Ok(());
        }
        mapper.suspend(&disk.name())?;
        mapper.resume(&disk.name())
    }
}

fn clear(log: &mut RamDevice) -> Result<(), String> {
    log.write_at(0, &[0; 4096])
        .map_err(|error| log.context(error))?;
    log.discard(0, RAM_DEVICE_BYTES)
        .map_err(|error| log.context(error))
}

fn log_error(spec: &DiskSpec, error: &LogError) -> String {
    format!("node {} disk {}: {error}", spec.node, spec.path)
}
