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
    self, LogError, Plan, RAM_DEVICE_BYTES, RAM_DEVICES_PER_DISK, SECTOR_BYTES, Sectors, dm,
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
    // BLKDISCARD, or a device-mapper buffer at least as long as the
    // `data_size` it declares.
    let result = unsafe { libc::ioctl(file.as_raw_fd(), request as _, argument) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

struct Mapper {
    control: File,
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
    ) -> Result<PathBuf, String> {
        let reply = self.call(
            dm::DEV_CREATE,
            dm::header(name, 0)?,
            &format!("create {name}"),
        )?;
        let (major, minor) = dm::device(&reply);
        self.call(
            dm::TABLE_LOAD,
            dm::table(name, sectors, target, params)?,
            &format!("load {name}"),
        )?;
        self.call(
            dm::DEV_SUSPEND,
            dm::header(name, 0)?,
            &format!("resume {name}"),
        )?;
        let path = PathBuf::from(format!("/dev/mapper/{name}"));
        mknodat(
            CWD,
            &path,
            FileType::BlockDevice,
            Mode::from_raw_mode(0o600),
            makedev(major, minor),
        )
        .map_err(|error| format!("{}: {error}", path.display()))?;
        Ok(path)
    }

    fn fail(&self, name: &str, sectors: u64) -> Result<(), String> {
        self.call(
            dm::DEV_SUSPEND,
            dm::header(name, dm::SUSPEND | dm::NOFLUSH | dm::SKIP_LOCKFS)?,
            &format!("suspend {name}"),
        )?;
        self.call(
            dm::TABLE_LOAD,
            dm::table(name, sectors, "error", "")?,
            &format!("load error table {name}"),
        )?;
        self.call(
            dm::DEV_SUSPEND,
            dm::header(name, 0)?,
            &format!("resume {name}"),
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

struct Disk {
    spec: DiskSpec,
    first: u32,
    generation: u64,
}

impl Disk {
    fn sectors(&self) -> u64 {
        self.spec.bytes / SECTOR_BYTES as u64
    }

    fn name(&self) -> String {
        format!("harmony-disk-{}-{}", self.spec.node, self.generation)
    }

    fn table(&self) -> String {
        format!("{RAM_MAJOR}:{} {RAM_MAJOR}:{}", self.first, self.first + 1)
    }

    fn attach(&self, mapper: &Mapper) -> Result<PathBuf, String> {
        mapper.create(&self.name(), self.sectors(), "log-writes", &self.table())
    }

    fn mount(&self, device: &Path) -> Result<(), String> {
        let path = Path::new(&self.spec.path);
        rustix::mount::mount(device, path, "ext4", MountFlags::empty(), MOUNT_DATA).map_err(
            |error| format!("mount {} on {}: {error}", device.display(), path.display()),
        )?;
        rustix::mount::mount_change(path, MountPropagationFlags::SHARED)
            .map_err(|error| format!("share mount {}: {error}", path.display()))
    }
}

pub struct Disks {
    mapper: Option<Mapper>,
    disks: Vec<Disk>,
}

impl Disks {
    pub fn create(specs: &[DiskSpec], uid: u32, gid: u32) -> Result<Self, String> {
        if specs.is_empty() {
            return Ok(Self {
                mapper: None,
                disks: Vec::new(),
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
            let disk = Disk {
                spec: spec.clone(),
                first,
                generation: 0,
            };
            let device = disk.attach(&mapper)?;
            let status = Command::new(MKFS[0])
                .args(&MKFS[1..])
                .arg("-E")
                .arg(format!("{MKFS_EXTENDED},root_owner={uid}:{gid}"))
                .arg(&device)
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
            std::fs::create_dir_all(&spec.path)
                .map_err(|error| format!("{}: {error}", spec.path))?;
            disk.mount(&device)?;
            let lost = Path::new(&spec.path).join("lost+found");
            std::fs::remove_dir(&lost).map_err(|error| format!("{}: {error}", lost.display()))?;
            let root = File::open(&spec.path).map_err(|error| format!("{}: {error}", spec.path))?;
            rustix::fs::syncfs(&root).map_err(|error| format!("sync {}: {error}", spec.path))?;
            disks.push(disk);
        }
        Ok(Self {
            mapper: Some(mapper),
            disks,
        })
    }

    pub fn power_loss(&mut self, node: u16, choice: u64) -> Result<Option<Plan>, String> {
        let Some(mapper) = &self.mapper else {
            return Ok(None);
        };
        let Some(disk) = self.disks.iter_mut().find(|disk| disk.spec.node == node) else {
            return Ok(None);
        };
        let name = disk.name();
        mapper.fail(&name, disk.sectors())?;
        rustix::mount::unmount(Path::new(&disk.spec.path), UnmountFlags::DETACH)
            .map_err(|error| format!("unmount {}: {error}", disk.spec.path))?;
        mapper.remove(&name)?;

        let open = |device: u32| {
            RamDevice::open(device)
                .map_err(|error| format!("{}: {error}", ram_path(device).display()))
        };
        let mut data = open(disk.first)?;
        let mut log = open(disk.first + 1)?;
        let mut base = open(disk.first + 2)?;
        let entries = disk::read_log(&mut log, RAM_DEVICE_BYTES / SECTOR_BYTES as u64)
            .map_err(|error| log_error(&disk.spec, &error))?;
        let plan = disk::plan(&entries, disk::node_choice(choice, node));
        disk::rebuild(&plan, &mut log, &mut base, &mut data, disk.sectors())
            .map_err(|error| log_error(&disk.spec, &error))?;
        log.write_at(0, &[0; 4096])
            .map_err(|error| log.context(error))?;
        match log.discard(RAM_DEVICE_BYTES) {
            Ok(()) => {}
            Err(error) if error.raw_os_error() == Some(libc::EOPNOTSUPP) => {}
            Err(error) => return Err(log.context(error)),
        }
        drop((data, log, base));

        disk.generation += 1;
        let device = disk.attach(mapper)?;
        disk.mount(&device)?;
        Ok(Some(plan))
    }
}

fn log_error(spec: &DiskSpec, error: &LogError) -> String {
    format!("node {} disk {}: {error}", spec.node, spec.path)
}
