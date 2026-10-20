// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rustix::fs::{CWD, FileType, Mode, makedev, mknodat};
use rustix::mount::{MountFlags, MountPropagationFlags, UnmountFlags};

use crate::bundle::DiskSpec;
use crate::disk::{
    self, DiskMode, FORMAT_MARK, Flip, LogEntry, LogError, Plan, RAM_DEVICE_BYTES,
    RAM_DEVICES_PER_DISK, SECTOR_BYTES, Sectors, dm,
};

const RAM_MAJOR: u32 = 1;
const MAPPER_CONTROL: &str = "/dev/mapper/control";
const MISC_MAJOR: u32 = 10;
const MAPPER_MINOR: u32 = 236;
const CHUNK_BYTES: usize = 64 * 1024;
const BLKDISCARD: u64 = 0x1277;
const MKFS: [&str; 7] = ["mkfs.ext4", "-F", "-q", "-b", "4096", "-m", "0"];
const MKFS_EXTENDED: &str = "lazy_itable_init=1,lazy_journal_init=1,nodiscard";
const MOUNT_DATA: &std::ffi::CStr = c"noinit_itable";

#[repr(C, align(4096))]
struct Chunk([u8; CHUNK_BYTES]);

pub struct RamDevice {
    file: File,
    chunk: Box<Chunk>,
    path: PathBuf,
}

impl RamDevice {
    fn open(index: u32) -> io::Result<Self> {
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
            .write(true)
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

    fn discard(&self, bytes: u64) -> io::Result<()> {
        let range = [0_u64, bytes];
        ioctl(&self.file, BLKDISCARD, range.as_ptr().cast_mut().cast())
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

fn ram_path(index: u32) -> PathBuf {
    PathBuf::from(format!("/dev/ram{index}"))
}

fn ioctl(file: &File, request: u64, argument: *mut libc::c_void) -> io::Result<()> {
    // SAFETY: `file` stays open for the call, and every caller passes a
    // request whose argument layout matches `argument`: a two-word range for
    // the block discard request, or a device-mapper buffer at least as long
    // as the `data_size` it declares.
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

    fn sync(&self) -> Result<(), String> {
        let root =
            File::open(&self.spec.path).map_err(|error| format!("{}: {error}", self.spec.path))?;
        rustix::fs::syncfs(&root).map_err(|error| format!("sync {}: {error}", self.spec.path))
    }

    fn format(&self, mapper: &Mapper, device: &Path, uid: u32, gid: u32) -> Result<(), String> {
        let status = Command::new(MKFS[0])
            .args(&MKFS[1..])
            .arg("-E")
            .arg(format!("{MKFS_EXTENDED},root_owner={uid}:{gid}"))
            .arg(device)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()
            .map_err(|error| {
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

    fn cut(&self, mapper: &Mapper) -> Result<Vec<LogEntry>, String> {
        let name = self.name();
        let log_name = self.log_name();
        mapper.fail(&log_name, self.sectors())?;
        mapper.fail(&name, self.sectors())?;
        rustix::mount::unmount(Path::new(&self.spec.path), UnmountFlags::DETACH)
            .map_err(|error| format!("unmount {}: {error}", self.spec.path))?;
        mapper.remove(&name)?;
        mapper.remove(&log_name)?;
        let mut log = self.open(1)?;
        disk::read_log(&mut log, RAM_DEVICE_BYTES / SECTOR_BYTES as u64)
            .map_err(|error| log_error(&self.spec, &error))
    }

    fn rebuild(&mut self, plan: &Plan, flip: Option<Flip>) -> Result<(), String> {
        let mut data = self.open(0)?;
        let mut log = self.open(1)?;
        let mut base = self.open(2)?;
        disk::rebuild(plan, &mut log, &mut base, &mut data, self.sectors())
            .map_err(|error| log_error(&self.spec, &error))?;
        if let Some(flip) = flip {
            flip.apply(&mut data).map_err(|error| data.context(error))?;
            flip.apply(&mut base).map_err(|error| base.context(error))?;
        }
        log.write_at(0, &[0; 4096])
            .map_err(|error| log.context(error))?;
        match log.discard(RAM_DEVICE_BYTES) {
            Ok(()) => {}
            Err(error) if error.raw_os_error() == Some(libc::EOPNOTSUPP) => {}
            Err(error) => return Err(log.context(error)),
        }
        self.generation += 1;
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
        let plan = disk::plan(&entries, disk::node_choice(choice, node));
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
        let flip = disk::corruption(&entries, disk::node_choice(choice, node));
        disk.rebuild(&disk::plan_all(&entries), flip)?;
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

    pub fn set_mode(&mut self, node: u16, mode: DiskMode) -> Result<bool, String> {
        let Some((mapper, disk)) = self.find(node) else {
            return Ok(false);
        };
        disk.mode = mode;
        disk.apply_mode(mapper)?;
        Ok(true)
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

fn log_error(spec: &DiskSpec, error: &LogError) -> String {
    format!("node {} disk {}: {error}", spec.node, spec.path)
}
