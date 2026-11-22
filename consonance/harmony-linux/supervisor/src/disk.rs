// SPDX-License-Identifier: AGPL-3.0-or-later

use std::io;

pub const SECTOR_BYTES: usize = 512;
pub const MIN_DISK_BYTES: u64 = 16 << 20;
pub const MAX_DISK_BYTES: u64 = RAM_DEVICE_BYTES;
pub const RAM_DEVICE_BYTES: u64 = 1 << 30;
pub const RAM_DEVICES: u32 = 16;
pub const RAM_DEVICES_PER_DISK: u32 = 3;
pub const MAX_DISKS: usize = (RAM_DEVICES / RAM_DEVICES_PER_DISK) as usize;

const LOG_MAGIC: u64 = 0x006a_7366_7773_6872;
const LOG_VERSION: u64 = 1;
const FLUSH: u64 = 1 << 0;
const FUA: u64 = 1 << 1;
const DISCARD: u64 = 1 << 2;
const MARK: u64 = 1 << 3;
const METADATA: u64 = 1 << 4;
const END_MARK: &[u8] = b"dm-log-writes-end";
const COPY_SECTORS: u64 = 128;
const PAGE_SECTORS: u64 = 8;
const GOLDEN: u64 = 0x9e37_79b9_7f4a_7c15;
pub const FORMAT_MARK: &str = "harmony-formatted";
pub const DRAIN_MARK: &str = "harmony-drain";
pub const MIN_STALL_MILLIS: u64 = 50;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DiskMode {
    #[default]
    Normal,
    Error,
    Stall {
        hold_nanos: u64,
    },
}

impl DiskMode {
    #[must_use]
    pub fn table(self, device: &str) -> (&'static str, String) {
        match self {
            Self::Normal => ("linear", format!("{device} 0")),
            Self::Error => ("error", String::new()),
            Self::Stall { hold_nanos } => {
                ("delay", format!("{device} 0 {}", stall_millis(hold_nanos)))
            }
        }
    }

    #[must_use]
    pub fn describe(self) -> String {
        match self {
            Self::Normal => "serves requests".to_owned(),
            Self::Error => "fails every request".to_owned(),
            Self::Stall { hold_nanos } => {
                format!("stalls every request for {} ms", stall_millis(hold_nanos))
            }
        }
    }
}

fn stall_millis(hold_nanos: u64) -> u64 {
    hold_nanos
        .div_ceil(1_000_000)
        .clamp(MIN_STALL_MILLIS, u64::from(u32::MAX))
}

#[must_use]
pub fn parse_size(text: &str) -> Option<u64> {
    let (digits, shift) = match text.as_bytes().last()? {
        b'K' => (&text[..text.len() - 1], 10),
        b'M' => (&text[..text.len() - 1], 20),
        b'G' => (&text[..text.len() - 1], 30),
        _ => (text, 0),
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let bytes = digits.parse::<u64>().ok()?.checked_mul(1 << shift)?;
    ((MIN_DISK_BYTES..=MAX_DISK_BYTES).contains(&bytes) && bytes.is_multiple_of(4096))
        .then_some(bytes)
}

#[must_use]
pub fn node_choice(choice: u64, node: u16) -> u64 {
    if choice == 0 {
        return 0;
    }
    mix((choice ^ u64::from(node)).wrapping_add(GOLDEN))
}

fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

struct Draw(u64);

impl Draw {
    fn below(&mut self, bound: u64) -> u64 {
        self.0 = self.0.wrapping_add(GOLDEN);
        mix(self.0) % bound
    }
}

pub trait Sectors {
    fn read_at(&mut self, sector: u64, buffer: &mut [u8]) -> io::Result<()>;
    fn write_at(&mut self, sector: u64, buffer: &[u8]) -> io::Result<()>;
}

#[derive(Clone, Debug, Default)]
pub struct Memory(pub Vec<u8>);

impl Sectors for Memory {
    fn read_at(&mut self, sector: u64, buffer: &mut [u8]) -> io::Result<()> {
        buffer.copy_from_slice(self.range(sector, buffer.len())?);
        Ok(())
    }

    fn write_at(&mut self, sector: u64, buffer: &[u8]) -> io::Result<()> {
        let at = self.start(sector)?;
        self.0
            .get_mut(at..at.saturating_add(buffer.len()))
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?
            .copy_from_slice(buffer);
        Ok(())
    }
}

impl Memory {
    fn start(&self, sector: u64) -> io::Result<usize> {
        usize::try_from(sector)
            .ok()
            .and_then(|sector| sector.checked_mul(SECTOR_BYTES))
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))
    }

    fn range(&self, sector: u64, bytes: usize) -> io::Result<&[u8]> {
        let at = self.start(sector)?;
        self.0
            .get(at..at.saturating_add(bytes))
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LogEntry {
    pub sector: u64,
    pub sectors: u64,
    pub flags: u64,
    pub data: u64,
}

impl LogEntry {
    #[must_use]
    pub fn changes_data(self) -> bool {
        self.flags & MARK == 0 && self.sectors != 0
    }

    #[must_use]
    pub fn flushes(self) -> bool {
        self.flags & FLUSH != 0
    }

    #[must_use]
    pub fn forced(self) -> bool {
        self.flags & FUA != 0
    }

    #[must_use]
    pub fn discards(self) -> bool {
        self.flags & DISCARD != 0
    }

    #[must_use]
    pub fn marks(self) -> bool {
        self.flags & MARK != 0
    }

    #[must_use]
    pub fn waits(self) -> bool {
        self.changes_data() && !self.forced()
    }

    #[must_use]
    pub fn plain(self) -> bool {
        self.flags & (FLUSH | FUA | DISCARD | MARK | METADATA) == 0 && self.sectors != 0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error("write log: {0}")]
    Io(#[from] io::Error),
    #[error("the write log has no dm-log-writes super block")]
    NoLog,
    #[error("the write log uses {0}-byte sectors")]
    SectorSize(u32),
    #[error("the write log does not end with the dm-log-writes end mark; the log device is full")]
    Incomplete,
    #[error("write log entry {0} lies outside its device")]
    OutOfRange(usize),
}

fn word(bytes: &[u8], at: usize) -> u64 {
    let mut word = [0; 8];
    word.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(word)
}

fn names(header: &[u8], mark: &[u8]) -> bool {
    word(header, 16) & MARK != 0
        && usize::try_from(word(header, 24)).is_ok_and(|length| length == mark.len())
        && header.get(32..32 + mark.len()) == Some(mark)
}

fn super_block(log: &mut impl Sectors) -> Result<(u64, u64), LogError> {
    let mut header = [0_u8; SECTOR_BYTES];
    log.read_at(0, &mut header)?;
    if word(&header, 0) != LOG_MAGIC || word(&header, 8) != LOG_VERSION {
        return Err(LogError::NoLog);
    }
    let sector_size = u32::from_le_bytes([header[24], header[25], header[26], header[27]]);
    if !sector_size.is_power_of_two() || !(512..=4096).contains(&sector_size) {
        return Err(LogError::SectorSize(sector_size));
    }
    Ok((
        word(&header, 16),
        u64::from(sector_size) / SECTOR_BYTES as u64,
    ))
}

fn drain_end(log: &mut impl Sectors, last_sector: u64) -> Result<Option<(u64, u64)>, LogError> {
    let scale = match super_block(log) {
        Ok((_, scale)) => scale,
        Err(LogError::NoLog) => return Ok(None),
        Err(error) => return Err(error),
    };
    let end = last_sector.checked_add(1).map_or(0, |end| end / scale);
    if end < 2 {
        return Ok(None);
    }
    let mut header = [0_u8; SECTOR_BYTES];
    log.read_at((end - 1) * scale, &mut header)?;
    Ok(names(&header, DRAIN_MARK.as_bytes()).then_some((end, scale)))
}

pub fn drain_marked(log: &mut impl Sectors, last_sector: u64) -> Result<bool, LogError> {
    Ok(drain_end(log, last_sector)?.is_some())
}

pub fn drain_logged(
    log: &mut impl Sectors,
    logged_entries: u64,
    last_sector: u64,
) -> Result<bool, LogError> {
    if logged_entries == 0 {
        return Ok(false);
    }
    let Some((end, scale)) = drain_end(log, last_sector)? else {
        return Ok(false);
    };
    let mut header = [0_u8; SECTOR_BYTES];
    let mut at = 1_u64;
    for _ in 1..logged_entries {
        log.read_at(at * scale, &mut header)?;
        let logged = if word(&header, 16) & DISCARD != 0 {
            0
        } else {
            word(&header, 8)
        };
        at = at.saturating_add(1).saturating_add(logged);
        if at >= end {
            return Ok(false);
        }
    }
    Ok(at + 1 == end)
}

pub fn read_log(log: &mut impl Sectors, log_sectors: u64) -> Result<Vec<LogEntry>, LogError> {
    let (count, scale) = super_block(log)?;
    let mut header = [0_u8; SECTOR_BYTES];
    let log_sectors = log_sectors / scale;
    let mut entries = Vec::new();
    let mut at = 1_u64;
    let mut end_mark = false;
    for index in 0..count {
        let index = usize::try_from(index).map_err(|_| LogError::OutOfRange(usize::MAX))?;
        if at >= log_sectors {
            return Err(LogError::OutOfRange(index));
        }
        log.read_at(at * scale, &mut header)?;
        let logged_sectors = word(&header, 8);
        let entry = LogEntry {
            sector: word(&header, 0).saturating_mul(scale),
            sectors: logged_sectors.saturating_mul(scale),
            flags: word(&header, 16),
            data: (at + 1) * scale,
        };
        end_mark = names(&header, END_MARK);
        let logged = if entry.discards() { 0 } else { logged_sectors };
        at = at
            .checked_add(1)
            .and_then(|at| at.checked_add(logged))
            .filter(|&end| end <= log_sectors)
            .ok_or(LogError::OutOfRange(index))?;
        if !names(&header, DRAIN_MARK.as_bytes()) {
            entries.push(entry);
        }
    }
    if !end_mark {
        return Err(LogError::Incomplete);
    }
    Ok(entries)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Plan {
    pub apply: Vec<LogEntry>,
    pub restore: Vec<LogEntry>,
    pub entries: usize,
    pub durable: usize,
    pub kept: usize,
}

impl Plan {
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "{} log entries, {} up to the last flush, {} of {} later unforced writes kept",
            self.entries,
            self.durable,
            self.kept,
            self.kept + self.restore.len()
        )
    }
}

fn durable(entries: &[LogEntry]) -> usize {
    entries
        .iter()
        .rposition(|entry| entry.flushes())
        .map_or(0, |index| index + 1)
}

#[must_use]
pub fn plan(entries: &[LogEntry], choice: u64) -> Plan {
    let durable = durable(entries);
    let unforced = entries[durable..]
        .iter()
        .filter(|entry| entry.changes_data() && !entry.forced())
        .count();
    keep(
        entries,
        durable,
        usize::try_from(choice % (unforced as u64 + 1)).unwrap_or(0),
    )
}

pub fn journal_failed(sync: std::io::Result<()>) -> std::io::Result<bool> {
    match sync {
        Ok(()) => Ok(false),
        Err(error) if matches!(error.raw_os_error(), Some(libc::EIO | libc::EROFS)) => Ok(true),
        Err(error) => Err(error),
    }
}

#[must_use]
pub fn emergency_read_only(mountinfo: &str, path: &str) -> bool {
    mountinfo
        .lines()
        .filter_map(|line| {
            let mount_point = line.split(' ').nth(4)?;
            let (_, tail) = line.split_once(" - ")?;
            let options = tail.split(' ').nth(2)?;
            (mount_point == path).then_some(options)
        })
        .next_back()
        .is_some_and(|options| {
            options
                .split(',')
                .any(|option| option == "emergency_ro" || option == "shutdown")
        })
}

#[must_use]
pub fn plan_all(entries: &[LogEntry]) -> Plan {
    keep(entries, durable(entries), usize::MAX)
}

fn keep(entries: &[LogEntry], durable: usize, kept: usize) -> Plan {
    let mut apply: Vec<LogEntry> = entries[..durable]
        .iter()
        .copied()
        .filter(|entry| entry.changes_data())
        .collect();
    let mut restore = Vec::new();
    let mut seen = 0;
    for &entry in entries[durable..]
        .iter()
        .filter(|entry| entry.changes_data())
    {
        if entry.forced() {
            apply.push(entry);
        } else {
            if seen < kept {
                apply.push(entry);
            } else {
                restore.push(entry);
            }
            seen += 1;
        }
    }
    Plan {
        apply,
        restore,
        entries: entries.len(),
        durable,
        kept: kept.min(seen),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Flip {
    pub sector: u64,
    pub byte: usize,
    pub bit: u8,
}

impl Flip {
    #[must_use]
    pub fn describe(self) -> String {
        format!(
            "bit {} of byte {} in sector {} flipped",
            self.bit, self.byte, self.sector
        )
    }

    pub fn apply(self, device: &mut impl Sectors) -> io::Result<()> {
        let page = self.sector - self.sector % PAGE_SECTORS;
        let mut bytes = [0_u8; PAGE_SECTORS as usize * SECTOR_BYTES];
        device.read_at(page, &mut bytes)?;
        let at = usize::try_from(self.sector - page).unwrap_or(0) * SECTOR_BYTES + self.byte;
        bytes[at] ^= 1 << self.bit;
        device.write_at(page, &bytes)
    }
}

#[must_use]
pub fn corruption(entries: &[LogEntry], choice: u64) -> Option<Flip> {
    flip(&[], entries, choice)
}

fn flip(folded: &[(u64, u64)], entries: &[LogEntry], choice: u64) -> Option<Flip> {
    let formatted = entries
        .len()
        .checked_sub(1)
        .and_then(|last| entries[..last].iter().rposition(|entry| entry.marks()));
    let folded = if formatted.is_some() { &[] } else { folded };
    let written = folded
        .iter()
        .copied()
        .chain(
            entries[formatted.map_or(0, |index| index + 1)..]
                .iter()
                .filter(|entry| entry.plain())
                .map(|entry| (entry.sector, entry.sectors)),
        )
        .collect::<Vec<_>>();
    let mut draw = Draw(choice);
    let &(sector, sectors) =
        written.get(usize::try_from(draw.below(written.len().max(1) as u64)).ok()?)?;
    Some(Flip {
        sector: sector + draw.below(sectors),
        byte: usize::try_from(draw.below(SECTOR_BYTES as u64)).ok()?,
        bit: u8::try_from(draw.below(8)).ok()?,
    })
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Folded {
    entries: usize,
    flushed: usize,
    written: Vec<(u64, u64)>,
    waiting: Vec<LogEntry>,
    held: Vec<u8>,
}

impl Folded {
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "{} log entries folded into the replay base, {} writes waiting for a flush kept with {} bytes of data",
            self.entries,
            self.waiting.len(),
            self.held.len()
        )
    }

    fn held_sectors(&self) -> u64 {
        (self.held.len() / SECTOR_BYTES) as u64
    }

    fn order(&self, logged: &[LogEntry]) -> Vec<LogEntry> {
        let shift = self.held_sectors();
        let released = logged
            .iter()
            .position(|entry| entry.flushes() || entry.waits())
            .unwrap_or(logged.len());
        let moved = |entry: &LogEntry| LogEntry {
            data: entry.data + shift,
            ..*entry
        };
        logged[..released]
            .iter()
            .map(moved)
            .chain(self.waiting.iter().copied())
            .chain(logged[released..].iter().map(moved))
            .collect()
    }

    #[must_use]
    pub fn join(&self, logged: &[LogEntry]) -> Vec<LogEntry> {
        let Some((end, logged)) = logged.split_last() else {
            return self.order(logged);
        };
        let mut entries = self.order(logged);
        entries.push(LogEntry {
            data: end.data + self.held_sectors(),
            ..*end
        });
        entries
    }

    pub fn fold(
        &mut self,
        logged: &[LogEntry],
        log: &mut impl Sectors,
        base: &mut impl Sectors,
        device_sectors: u64,
    ) -> Result<(), LogError> {
        let entries = self.order(logged.split_last().map_or(logged, |(_, logged)| logged));
        let durable = durable(&entries);
        let settled = entries[durable..]
            .iter()
            .position(|entry| entry.waits())
            .map_or(entries.len(), |index| durable + index);
        let (folded, kept) = entries.split_at(settled);
        in_device(folded.iter().chain(kept), device_sectors)?;
        let mut source = Held {
            held: &self.held,
            log,
        };
        let mut buffer = vec![0_u8; COPY_SECTORS as usize * SECTOR_BYTES];
        for entry in folded.iter().filter(|entry| entry.changes_data()) {
            copy(entry, &mut source, base, &mut buffer)?;
        }
        for entry in folded {
            if entry.marks() {
                self.written.clear();
            } else if entry.plain() {
                self.written.push((entry.sector, entry.sectors));
            }
        }
        let mut held = Vec::new();
        let mut waiting = Vec::with_capacity(kept.len());
        for &entry in kept {
            let mut entry = entry;
            if entry.changes_data() && !entry.discards() {
                let start = held.len();
                held.resize(start + entry.sectors as usize * SECTOR_BYTES, 0);
                source.read_at(entry.data, &mut held[start..])?;
                entry.data = (start / SECTOR_BYTES) as u64;
            }
            waiting.push(entry);
        }
        if durable > 0 {
            self.flushed = self.entries + durable;
        }
        self.entries += settled;
        self.held = held;
        self.waiting = waiting;
        Ok(())
    }

    #[must_use]
    pub fn plan(&self, entries: &[LogEntry], choice: u64) -> Plan {
        self.counted(plan(entries, choice))
    }

    #[must_use]
    pub fn plan_all(&self, entries: &[LogEntry]) -> Plan {
        self.counted(plan_all(entries))
    }

    fn counted(&self, mut plan: Plan) -> Plan {
        plan.durable += if plan.durable == 0 {
            self.flushed
        } else {
            self.entries
        };
        plan.entries += self.entries;
        plan
    }

    #[must_use]
    pub fn corruption(&self, entries: &[LogEntry], choice: u64) -> Option<Flip> {
        flip(&self.written, entries, choice)
    }

    pub fn rebuild(
        &self,
        plan: &Plan,
        log: &mut impl Sectors,
        base: &mut impl Sectors,
        data: &mut impl Sectors,
        device_sectors: u64,
    ) -> Result<(), LogError> {
        rebuild(
            plan,
            &mut Held {
                held: &self.held,
                log,
            },
            base,
            data,
            device_sectors,
        )
    }
}

struct Held<'a, S> {
    held: &'a [u8],
    log: &'a mut S,
}

impl<S: Sectors> Sectors for Held<'_, S> {
    fn read_at(&mut self, sector: u64, buffer: &mut [u8]) -> io::Result<()> {
        let held = (self.held.len() / SECTOR_BYTES) as u64;
        if sector >= held {
            return self.log.read_at(sector - held, buffer);
        }
        let at = usize::try_from(sector).unwrap_or(usize::MAX) * SECTOR_BYTES;
        let bytes = self
            .held
            .get(at..at + buffer.len())
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        buffer.copy_from_slice(bytes);
        Ok(())
    }

    fn write_at(&mut self, _: u64, _: &[u8]) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

fn in_device<'a>(
    entries: impl Iterator<Item = &'a LogEntry>,
    device_sectors: u64,
) -> Result<(), LogError> {
    for (index, entry) in entries.enumerate() {
        if entry.changes_data()
            && entry
                .sector
                .checked_add(entry.sectors)
                .is_none_or(|end| end > device_sectors)
        {
            return Err(LogError::OutOfRange(index));
        }
    }
    Ok(())
}

fn copy(
    entry: &LogEntry,
    log: &mut impl Sectors,
    base: &mut impl Sectors,
    buffer: &mut [u8],
) -> io::Result<()> {
    let mut done = 0;
    while done < entry.sectors {
        let count = (entry.sectors - done).min(COPY_SECTORS);
        let chunk = &mut buffer[..count as usize * SECTOR_BYTES];
        if entry.discards() {
            chunk.fill(0);
        } else {
            log.read_at(entry.data + done, chunk)?;
        }
        base.write_at(entry.sector + done, chunk)?;
        done += count;
    }
    Ok(())
}

pub fn rebuild(
    plan: &Plan,
    log: &mut impl Sectors,
    base: &mut impl Sectors,
    data: &mut impl Sectors,
    device_sectors: u64,
) -> Result<(), LogError> {
    let mut buffer = vec![0_u8; COPY_SECTORS as usize * SECTOR_BYTES];
    in_device(plan.apply.iter().chain(&plan.restore), device_sectors)?;
    for entry in &plan.apply {
        copy(entry, log, base, &mut buffer)?;
    }
    for entry in &plan.restore {
        let mut done = 0;
        while done < entry.sectors {
            let count = (entry.sectors - done).min(COPY_SECTORS);
            let chunk = &mut buffer[..count as usize * SECTOR_BYTES];
            base.read_at(entry.sector + done, chunk)?;
            data.write_at(entry.sector + done, chunk)?;
            done += count;
        }
    }
    Ok(())
}

pub mod dm {
    pub const IOCTL_BYTES: usize = 312;
    pub const DEV_CREATE: u8 = 3;
    pub const DEV_REMOVE: u8 = 4;
    pub const DEV_SUSPEND: u8 = 6;
    pub const TABLE_LOAD: u8 = 9;
    pub const TARGET_MSG: u8 = 14;
    pub const SUSPEND: u32 = 1 << 1;
    pub const SKIP_LOCKFS: u32 = 1 << 10;
    pub const NOFLUSH: u32 = 1 << 11;
    pub const DEFERRED_REMOVE: u32 = 1 << 17;
    pub const TABLE_STATUS: u8 = 12;
    pub const STATUS_BYTES: usize = IOCTL_BYTES + 512;
    const BUFFER_FULL: u32 = 1 << 8;
    const NAME_BYTES: usize = 128;
    const SPEC_BYTES: usize = 40;
    const TYPE_BYTES: usize = 16;

    pub fn status(name: &str) -> Result<Vec<u8>, String> {
        let mut out = header(name, 0)?;
        out.resize(STATUS_BYTES, 0);
        set_u32(&mut out, 12, STATUS_BYTES as u32);
        Ok(out)
    }

    pub fn status_line(reply: &[u8]) -> Result<&str, String> {
        let word = |at: usize| {
            reply
                .get(at..at + 4)
                .map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        };
        if word(28).is_some_and(|flags| flags & BUFFER_FULL != 0) {
            return Err("device-mapper status does not fit its buffer".to_owned());
        }
        if word(20) != Some(1) {
            return Err("device-mapper status does not hold one target".to_owned());
        }
        let line = word(16)
            .and_then(|start| usize::try_from(start).ok())
            .and_then(|start| reply.get(start + SPEC_BYTES..))
            .and_then(|tail| tail.split(|&byte| byte == 0).next())
            .ok_or("device-mapper status reply is truncated")?;
        std::str::from_utf8(line).map_err(|error| format!("device-mapper status: {error}"))
    }

    #[must_use]
    pub fn logged_entries(line: &str) -> Option<u64> {
        line.split(' ').next()?.parse().ok()
    }

    #[must_use]
    pub fn logged_sectors(line: &str) -> Option<u64> {
        line.split(' ').nth(1)?.parse().ok()
    }

    #[must_use]
    pub fn logging_disabled(line: &str) -> bool {
        line.split(' ').nth(2) == Some("logging_disabled")
    }

    #[must_use]
    pub fn request(command: u8) -> u64 {
        (3 << 30) | ((IOCTL_BYTES as u64) << 16) | (0xfd << 8) | u64::from(command)
    }

    pub fn header(name: &str, flags: u32) -> Result<Vec<u8>, String> {
        if name.is_empty() || name.len() >= NAME_BYTES || name.contains('\0') {
            return Err(format!("device-mapper name {name:?} is invalid"));
        }
        let mut out = vec![0_u8; IOCTL_BYTES];
        for (index, part) in [4_u32, 0, 0].into_iter().enumerate() {
            out[index * 4..index * 4 + 4].copy_from_slice(&part.to_le_bytes());
        }
        set_u32(&mut out, 12, IOCTL_BYTES as u32);
        set_u32(&mut out, 16, IOCTL_BYTES as u32);
        set_u32(&mut out, 28, flags);
        out[48..48 + name.len()].copy_from_slice(name.as_bytes());
        Ok(out)
    }

    pub fn table(name: &str, sectors: u64, target: &str, params: &str) -> Result<Vec<u8>, String> {
        if target.len() >= TYPE_BYTES || params.contains('\0') {
            return Err(format!("device-mapper target {target:?} is invalid"));
        }
        let mut out = header(name, 0)?;
        let spec = out.len();
        out.resize(spec + SPEC_BYTES, 0);
        out[spec + 8..spec + 16].copy_from_slice(&sectors.to_le_bytes());
        out[spec + 24..spec + 24 + target.len()].copy_from_slice(target.as_bytes());
        out.extend_from_slice(params.as_bytes());
        out.push(0);
        out.resize(out.len().next_multiple_of(8), 0);
        let next = u32::try_from(out.len() - spec).map_err(|error| error.to_string())?;
        set_u32(&mut out, spec + 20, next);
        let size = u32::try_from(out.len()).map_err(|error| error.to_string())?;
        set_u32(&mut out, 12, size);
        set_u32(&mut out, 20, 1);
        Ok(out)
    }

    pub fn message(name: &str, text: &str) -> Result<Vec<u8>, String> {
        if text.contains('\0') {
            return Err(format!("device-mapper message {text:?} is invalid"));
        }
        let mut out = header(name, 0)?;
        out.extend_from_slice(&0_u64.to_le_bytes());
        out.extend_from_slice(text.as_bytes());
        out.push(0);
        out.resize(out.len().next_multiple_of(8), 0);
        let size = u32::try_from(out.len()).map_err(|error| error.to_string())?;
        set_u32(&mut out, 12, size);
        Ok(out)
    }

    #[must_use]
    pub fn device(reply: &[u8]) -> (u32, u32) {
        let mut word = [0; 8];
        word.copy_from_slice(&reply[40..48]);
        let dev = u64::from_le_bytes(word);
        let major = ((dev >> 8) & 0xfff) as u32;
        let minor = ((dev & 0xff) | ((dev >> 12) & 0xfff_ff00)) as u32;
        (major, minor)
    }

    fn set_u32(out: &mut [u8], at: usize, value: u32) {
        out[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sync_that_fails_with_eio_or_erofs_reports_a_failed_journal() {
        use std::io::{Error, ErrorKind};
        assert!(!journal_failed(Ok(())).unwrap());
        assert!(journal_failed(Err(Error::from_raw_os_error(libc::EIO))).unwrap());
        assert!(journal_failed(Err(Error::from_raw_os_error(libc::EROFS))).unwrap());
        let other = journal_failed(Err(Error::from_raw_os_error(libc::ENOENT))).unwrap_err();
        assert_eq!(other.kind(), ErrorKind::NotFound);
    }

    #[test]
    fn the_last_mount_on_a_path_reports_ext4_emergency_read_only() {
        let healthy = "36 25 254:5 / /tmp/tarantool/n3 rw,relatime shared:12 - ext4 /dev/mapper/harmony-disk-2-0 rw,noinit_itable\n";
        let aborted = "37 25 254:7 / /tmp/tarantool/n3 rw,relatime shared:13 - ext4 /dev/mapper/harmony-disk-2-1 rw,noinit_itable,emergency_ro\n";
        let other = "38 25 254:9 / /tmp/tarantool/n2 rw,relatime shared:14 - ext4 /dev/mapper/harmony-disk-1-0 rw,shutdown\n";
        assert!(!emergency_read_only(healthy, "/tmp/tarantool/n3"));
        assert!(emergency_read_only(aborted, "/tmp/tarantool/n3"));
        assert!(!emergency_read_only(
            &format!("{aborted}{healthy}"),
            "/tmp/tarantool/n3"
        ));
        assert!(emergency_read_only(
            &format!("{healthy}{aborted}"),
            "/tmp/tarantool/n3"
        ));
        assert!(!emergency_read_only(other, "/tmp/tarantool/n3"));
        assert!(emergency_read_only(other, "/tmp/tarantool/n2"));
    }

    impl Memory {
        fn sectors(count: u64) -> Self {
            Self(vec![0; count as usize * SECTOR_BYTES])
        }
    }

    #[derive(Debug)]
    enum Op {
        Write(u64, u8, u64),
        Fua(u64, u8, u64),
        Meta(u64, u8, u64),
        Commit(u64, u8, u64),
        Discard(u64, u64),
        Flush,
        Mark,
    }

    struct Recorder {
        log: Memory,
        data: Memory,
        next: u64,
        count: u64,
        unflushed: Vec<(u64, u64, u64, Vec<u8>)>,
    }

    impl Recorder {
        fn new() -> Self {
            Self {
                log: Memory::sectors(256),
                data: Memory::sectors(64),
                next: 1,
                count: 0,
                unflushed: Vec::new(),
            }
        }

        fn release(&mut self) {
            for (sector, sectors, flags, payload) in std::mem::take(&mut self.unflushed) {
                self.entry(sector, sectors, flags, &payload);
            }
        }

        fn clear_log(&mut self) {
            self.log = Memory::sectors(256);
            self.next = 1;
            self.count = 0;
        }

        fn entry(&mut self, sector: u64, sectors: u64, flags: u64, payload: &[u8]) {
            let mut header = [0_u8; SECTOR_BYTES];
            header[0..8].copy_from_slice(&sector.to_le_bytes());
            header[8..16].copy_from_slice(&sectors.to_le_bytes());
            header[16..24].copy_from_slice(&flags.to_le_bytes());
            if flags & MARK != 0 {
                header[24..32].copy_from_slice(&(payload.len() as u64).to_le_bytes());
                header[32..32 + payload.len()].copy_from_slice(payload);
            }
            self.log.write_at(self.next, &header).unwrap();
            self.next += 1;
            if flags & (MARK | DISCARD) == 0 && sectors != 0 {
                self.log.write_at(self.next, payload).unwrap();
                self.next += sectors;
            }
            self.count += 1;
        }

        fn apply(&mut self, op: &Op) {
            match *op {
                Op::Write(sector, fill, sectors)
                | Op::Fua(sector, fill, sectors)
                | Op::Meta(sector, fill, sectors)
                | Op::Commit(sector, fill, sectors) => {
                    let payload = vec![fill; sectors as usize * SECTOR_BYTES];
                    self.data.write_at(sector, &payload).unwrap();
                    match op {
                        Op::Fua(..) => self.entry(sector, sectors, FUA, &payload),
                        Op::Commit(..) => {
                            self.release();
                            self.entry(0, 0, FLUSH, &[]);
                            self.entry(sector, sectors, FUA, &payload);
                        }
                        Op::Meta(..) => self.unflushed.push((sector, sectors, METADATA, payload)),
                        _ => self.unflushed.push((sector, sectors, 0, payload)),
                    }
                }
                Op::Discard(sector, sectors) => {
                    self.data
                        .write_at(sector, &vec![0; sectors as usize * SECTOR_BYTES])
                        .unwrap();
                    self.unflushed.push((sector, sectors, DISCARD, Vec::new()));
                }
                Op::Flush => {
                    self.release();
                    self.entry(0, 0, FLUSH, &[]);
                }
                Op::Mark => self.entry(0, 0, MARK, FORMAT_MARK.as_bytes()),
            }
        }

        fn super_block(&mut self) {
            let mut header = [0_u8; SECTOR_BYTES];
            header[0..8].copy_from_slice(&LOG_MAGIC.to_le_bytes());
            header[8..16].copy_from_slice(&LOG_VERSION.to_le_bytes());
            header[16..24].copy_from_slice(&self.count.to_le_bytes());
            header[24..28].copy_from_slice(&(SECTOR_BYTES as u32).to_le_bytes());
            self.log.write_at(0, &header).unwrap();
        }

        fn drain(&mut self) {
            self.entry(0, 0, MARK, DRAIN_MARK.as_bytes());
            self.super_block();
        }

        fn teardown(&mut self) {
            self.release();
            self.entry(0, 0, MARK, END_MARK);
            self.super_block();
        }

        fn finish(&mut self) {
            self.drain();
            self.teardown();
        }
    }

    fn image<'a>(ops: impl IntoIterator<Item = &'a Op>) -> Vec<u8> {
        let mut recorder = Recorder::new();
        for op in ops {
            recorder.apply(op);
        }
        recorder.data.0
    }

    fn logged(ops: &[Op]) -> (Recorder, Vec<LogEntry>) {
        let mut recorder = Recorder::new();
        for op in ops {
            recorder.apply(op);
        }
        recorder.finish();
        let entries = read_log(&mut recorder.log, 256).unwrap();
        (recorder, entries)
    }

    fn crash(ops: &[Op], choice: u64) -> (Plan, Vec<u8>, Vec<u8>) {
        crash_with(ops, |entries| plan(entries, choice))
    }

    fn crash_with(ops: &[Op], planner: impl Fn(&[LogEntry]) -> Plan) -> (Plan, Vec<u8>, Vec<u8>) {
        let (mut recorder, entries) = logged(ops);
        let plan = planner(&entries);
        let mut base = Memory::sectors(64);
        rebuild(&plan, &mut recorder.log, &mut base, &mut recorder.data, 64).unwrap();
        (plan, base.0, recorder.data.0)
    }

    fn history() -> Vec<Op> {
        vec![
            Op::Write(0, 1, 4),
            Op::Write(8, 2, 2),
            Op::Flush,
            Op::Write(0, 3, 1),
            Op::Fua(20, 4, 1),
            Op::Write(8, 5, 4),
            Op::Discard(0, 2),
            Op::Write(30, 6, 1),
        ]
    }

    fn random_history(seed: u64, length: usize) -> Vec<Op> {
        let mut draw = Draw(seed);
        (0..length)
            .map(|_| {
                let sectors = 1 + draw.below(4);
                let sector = draw.below(64 - sectors + 1);
                let fill = 1 + draw.below(255) as u8;
                match draw.below(16) {
                    0..=6 => Op::Write(sector, fill, sectors),
                    7..=8 => Op::Meta(sector, fill, sectors),
                    9..=10 => Op::Fua(sector, fill, sectors),
                    11 => Op::Commit(sector, fill, sectors),
                    12 => Op::Discard(sector, sectors),
                    13..=14 => Op::Flush,
                    _ => Op::Mark,
                }
            })
            .collect()
    }

    struct Compacted {
        recorder: Recorder,
        base: Memory,
        folded: Folded,
    }

    fn compacted(ops: &[Op], points: &[usize]) -> (Compacted, Vec<LogEntry>) {
        let mut disk = Compacted {
            recorder: Recorder::new(),
            base: Memory::sectors(64),
            folded: Folded::default(),
        };
        for index in 0..=ops.len() {
            for _ in points.iter().filter(|&&point| point == index) {
                disk.recorder.finish();
                let mut image = disk.recorder.log.clone();
                disk.recorder.clear_log();
                let logged = read_log(&mut image, 256).unwrap();
                disk.folded
                    .fold(&logged, &mut image, &mut disk.base, 64)
                    .unwrap();
            }
            if let Some(op) = ops.get(index) {
                disk.recorder.apply(op);
            }
        }
        disk.recorder.finish();
        let logged = read_log(&mut disk.recorder.log, 256).unwrap();
        let entries = disk.folded.join(&logged);
        (disk, entries)
    }

    fn shape(entries: &[LogEntry]) -> Vec<(u64, u64, u64)> {
        entries
            .iter()
            .map(|entry| (entry.sector, entry.sectors, entry.flags))
            .collect()
    }

    fn assert_compaction_changes_nothing(ops: &[Op], points: &[usize], choices: &[u64]) {
        let context = format!("{ops:?} compacted before ops {points:?}");
        let (mut whole, expected) = logged(ops);
        let (mut disk, entries) = compacted(ops, points);
        let folded = expected.len() - entries.len();
        assert_eq!(disk.folded.entries, folded, "{context}");
        assert_eq!(shape(&entries), shape(&expected[folded..]), "{context}");
        for (entry, original) in entries.iter().zip(&expected[folded..]) {
            if entry.changes_data() && !entry.discards() {
                let mut kept = vec![0; entry.sectors as usize * SECTOR_BYTES];
                let mut written = kept.clone();
                Held {
                    held: &disk.folded.held,
                    log: &mut disk.recorder.log,
                }
                .read_at(entry.data, &mut kept)
                .unwrap();
                whole.log.read_at(original.data, &mut written).unwrap();
                assert!(kept == written, "{context}: data of {entry:?}");
            }
        }
        let plans = choices
            .iter()
            .map(|&choice| (plan(&expected, choice), disk.folded.plan(&entries, choice)))
            .chain([(plan_all(&expected), disk.folded.plan_all(&entries))]);
        for (want, plan) in plans {
            assert_eq!(
                (plan.entries, plan.durable, plan.kept),
                (want.entries, want.durable, want.kept),
                "{context}"
            );
            assert_eq!(shape(&plan.restore), shape(&want.restore), "{context}");
            let mut want_base = Memory::sectors(64);
            let mut want_data = whole.data.clone();
            rebuild(&want, &mut whole.log, &mut want_base, &mut want_data, 64).unwrap();
            let mut base = disk.base.clone();
            let mut data = disk.recorder.data.clone();
            disk.folded
                .rebuild(&plan, &mut disk.recorder.log, &mut base, &mut data, 64)
                .unwrap();
            assert!(base.0 == want_base.0, "{context}: base after {plan:?}");
            assert!(data.0 == want_data.0, "{context}: data after {plan:?}");
        }
        for &choice in choices {
            assert_eq!(
                disk.folded.corruption(&entries, choice),
                corruption(&expected, choice),
                "{context}: corruption choice {choice}"
            );
        }
    }

    #[test]
    fn a_power_loss_keeps_flushed_and_forced_writes() {
        let ops = history();
        let (plan, base, data) = crash(&ops, 0);
        assert_eq!(plan.entries, 9);
        assert_eq!(plan.durable, 3);
        assert_eq!(plan.kept, 0);
        assert_eq!(plan.restore.len(), 4);
        assert_eq!(base, image(ops[..3].iter().chain([&ops[4]])));
        assert_eq!(data, base);
    }

    #[test]
    fn the_choice_keeps_a_prefix_of_the_unflushed_writes_in_order() {
        let ops = history();
        let unforced = [3, 5, 6, 7];
        for kept in 0..=4 {
            let (plan, base, data) = crash(&ops, kept as u64);
            assert_eq!(plan.kept, kept);
            let expected = image((0..ops.len()).filter_map(|index| {
                (index < 3 || index == 4 || unforced[..kept].contains(&index))
                    .then_some(&ops[index])
            }));
            assert_eq!(base, expected, "kept {kept}");
            assert_eq!(data, base, "kept {kept}");
        }
        assert_eq!(crash(&ops, 5).0.kept, 0, "the choice wraps around the tail");
        assert_eq!(crash(&ops, 4).1, image(&ops));
    }

    #[test]
    fn keeping_every_write_rebuilds_the_disk_as_it_was_written() {
        let ops = history();
        let (plan, base, data) = crash_with(&ops, plan_all);
        assert_eq!(plan.kept, 4);
        assert!(plan.restore.is_empty());
        assert_eq!(base, image(&ops));
        assert_eq!(data, base);
    }

    #[test]
    fn a_corruption_lands_in_a_plain_write_after_the_format_mark() {
        let ops = [
            Op::Write(0, 1, 4),
            Op::Flush,
            Op::Mark,
            Op::Meta(10, 2, 2),
            Op::Fua(20, 3, 1),
            Op::Write(30, 4, 2),
            Op::Flush,
            Op::Discard(40, 1),
            Op::Write(50, 5, 1),
        ];
        let (_, entries) = logged(&ops);
        let mut sectors = std::collections::BTreeSet::new();
        for choice in 0..200 {
            let flip = corruption(&entries, choice).expect("a plain write to corrupt");
            assert_eq!(corruption(&entries, choice), Some(flip));
            assert!(flip.byte < SECTOR_BYTES && flip.bit < 8);
            sectors.insert(flip.sector);
        }
        assert_eq!(sectors, [30, 31, 50].into());
        let (_, before_mark) = logged(&ops[..2]);
        assert!(corruption(&before_mark, 3).is_some());
        let (_, after_mark) = logged(&ops[..5]);
        assert_eq!(corruption(&after_mark, 3), None);
    }

    #[test]
    fn a_flip_changes_one_bit_and_a_second_flip_restores_it() {
        let mut device = Memory::sectors(64);
        for (index, byte) in device.0.iter_mut().enumerate() {
            *byte = index as u8;
        }
        let before = device.0.clone();
        let flip = Flip {
            sector: 9,
            byte: 3,
            bit: 5,
        };
        flip.apply(&mut device).unwrap();
        let changed = (0..before.len())
            .filter(|&at| before[at] != device.0[at])
            .collect::<Vec<_>>();
        assert_eq!(changed, [9 * SECTOR_BYTES + 3]);
        assert_eq!(before[changed[0]] ^ device.0[changed[0]], 1 << 5);
        flip.apply(&mut device).unwrap();
        assert_eq!(device.0, before);
    }

    #[test]
    fn disk_modes_load_the_matching_device_mapper_table() {
        assert_eq!(
            DiskMode::Normal.table("253:4"),
            ("linear", "253:4 0".to_owned())
        );
        assert_eq!(DiskMode::Error.table("253:4"), ("error", String::new()));
        for (hold_nanos, millis) in [
            (1, 50),
            (120_000_000, 120),
            (120_000_001, 121),
            (u64::MAX, u64::from(u32::MAX)),
        ] {
            assert_eq!(
                DiskMode::Stall { hold_nanos }.table("253:4"),
                ("delay", format!("253:4 0 {millis}"))
            );
        }
    }

    #[test]
    fn a_log_without_a_flush_starts_from_the_base() {
        let ops = [Op::Write(0, 9, 2), Op::Write(4, 8, 1)];
        let (plan, base, data) = crash(&ops, 0);
        assert_eq!(plan.durable, 0);
        assert!(base.iter().all(|&byte| byte == 0));
        assert_eq!(data, base);
        let (_, base, _) = crash(&ops, 2);
        assert_eq!(base, image(&ops));
    }

    #[test]
    fn a_log_missing_its_end_mark_or_super_block_is_refused() {
        let mut recorder = Recorder::new();
        recorder.entry(0, 1, 0, &[1; SECTOR_BYTES]);
        assert!(matches!(
            read_log(&mut recorder.log, 256),
            Err(LogError::NoLog)
        ));
        recorder.count = 1;
        let mut header = [0_u8; SECTOR_BYTES];
        header[0..8].copy_from_slice(&LOG_MAGIC.to_le_bytes());
        header[8..16].copy_from_slice(&LOG_VERSION.to_le_bytes());
        header[16..24].copy_from_slice(&1_u64.to_le_bytes());
        header[24..28].copy_from_slice(&(SECTOR_BYTES as u32).to_le_bytes());
        recorder.log.write_at(0, &header).unwrap();
        assert!(matches!(
            read_log(&mut recorder.log, 256),
            Err(LogError::Incomplete)
        ));
        recorder.finish();
        assert!(matches!(
            read_log(&mut recorder.log, 2),
            Err(LogError::OutOfRange(_))
        ));
    }

    #[test]
    fn a_log_entry_outside_the_device_is_refused() {
        let mut recorder = Recorder::new();
        recorder.entry(62, 4, 0, &[1; 4 * SECTOR_BYTES]);
        recorder.finish();
        let entries = read_log(&mut recorder.log, 256).unwrap();
        let plan = plan(&entries, 1);
        let mut base = Memory::sectors(64);
        assert!(matches!(
            rebuild(&plan, &mut recorder.log, &mut base, &mut recorder.data, 64),
            Err(LogError::OutOfRange(0))
        ));
    }

    #[test]
    fn disk_sizes_are_whole_pages_within_the_ram_device() {
        assert_eq!(parse_size("16M"), Some(16 << 20));
        assert_eq!(parse_size("1G"), Some(1 << 30));
        assert_eq!(parse_size("20480K"), Some(20 << 20));
        assert_eq!(parse_size("16777216"), Some(16 << 20));
        for bad in ["", "M", "15M", "2G", "16m", "1.5G", "-16M", "16777217"] {
            assert_eq!(parse_size(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_zero_choice_keeps_no_unflushed_write_on_any_node() {
        assert_eq!(node_choice(0, 3), 0);
        assert_ne!(node_choice(5, 0), node_choice(5, 1));
        assert_eq!(node_choice(5, 1), node_choice(5, 1));
    }

    #[test]
    fn device_mapper_requests_follow_the_kernel_layout() {
        assert_eq!(dm::request(dm::DEV_CREATE), 0xc138_fd03);
        let create = dm::header("harmony-disk-0-1", dm::SUSPEND).unwrap();
        assert_eq!(create.len(), dm::IOCTL_BYTES);
        assert_eq!(&create[0..12], &[4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&create[48..64], b"harmony-disk-0-1");
        assert_eq!(create[28], 2);
        let table = dm::table("d", 32768, "log-writes", "1:0 1:1").unwrap();
        assert_eq!(table.len() % 8, 0);
        assert_eq!(
            u32::from_le_bytes(table[12..16].try_into().unwrap()) as usize,
            table.len()
        );
        assert_eq!(u32::from_le_bytes(table[20..24].try_into().unwrap()), 1);
        let spec = dm::IOCTL_BYTES;
        assert_eq!(
            u64::from_le_bytes(table[spec + 8..spec + 16].try_into().unwrap()),
            32768
        );
        assert_eq!(&table[spec + 24..spec + 34], b"log-writes");
        assert_eq!(&table[spec + 40..spec + 48], b"1:0 1:1\0");
        assert!(dm::header("", 0).is_err());
        assert!(dm::header(&"x".repeat(128), 0).is_err());
        let mut reply = vec![0_u8; dm::IOCTL_BYTES];
        let dev: u64 = 0x12 | (253 << 8) | (0x345 << 20);
        reply[40..48].copy_from_slice(&dev.to_le_bytes());
        assert_eq!(dm::device(&reply), (253, 0x34512));
        let message = dm::message("harmony-log-0-0", "mark harmony-formatted").unwrap();
        assert_eq!(message.len() % 8, 0);
        assert_eq!(
            u32::from_le_bytes(message[12..16].try_into().unwrap()) as usize,
            message.len()
        );
        assert_eq!(&message[spec..spec + 8], &[0; 8]);
        assert_eq!(&message[spec + 8..spec + 31], b"mark harmony-formatted\0");
        assert!(dm::message("d", "a\0b").is_err());
        assert_eq!(dm::request(dm::TARGET_MSG), 0xc138_fd0e);
    }

    #[test]
    fn a_log_writes_status_reply_gives_the_logged_sectors() {
        let request = dm::status("harmony-log-0-0").unwrap();
        assert_eq!(request.len(), dm::STATUS_BYTES);
        assert_eq!(
            u32::from_le_bytes(request[12..16].try_into().unwrap()) as usize,
            dm::STATUS_BYTES
        );
        assert_eq!(&request[48..63], b"harmony-log-0-0");
        assert_eq!(dm::request(dm::TABLE_STATUS), 0xc138_fd0c);
        let mut reply = request;
        reply[20..24].copy_from_slice(&1_u32.to_le_bytes());
        let line = b"812 131073 logging_disabled\0";
        let at = dm::IOCTL_BYTES + 40;
        reply[at..at + line.len()].copy_from_slice(line);
        let status = dm::status_line(&reply).unwrap();
        assert_eq!(status, "812 131073 logging_disabled");
        assert_eq!(dm::logged_sectors(status), Some(131_073));
        assert!(dm::logging_disabled(status));
        assert_eq!(dm::logged_sectors("3 24"), Some(24));
        assert!(!dm::logging_disabled("3 24"));
        assert_eq!(dm::logged_sectors("3"), None);
        let mut full = reply.clone();
        full[28..32].copy_from_slice(&(1_u32 << 8).to_le_bytes());
        assert!(dm::status_line(&full).is_err());
        let mut empty = reply.clone();
        empty[20..24].copy_from_slice(&0_u32.to_le_bytes());
        assert!(dm::status_line(&empty).is_err());
        let mut outside = reply;
        outside[16..20].copy_from_slice(&(dm::STATUS_BYTES as u32).to_le_bytes());
        assert!(dm::status_line(&outside).is_err());
    }

    #[test]
    fn a_write_waiting_for_a_flush_at_compaction_stays_behind_later_forced_writes() {
        let ops = [
            Op::Write(0, 1, 2),
            Op::Flush,
            Op::Write(4, 2, 1),
            Op::Mark,
            Op::Fua(4, 3, 1),
            Op::Write(8, 4, 1),
            Op::Fua(12, 5, 1),
            Op::Flush,
            Op::Write(16, 6, 1),
        ];
        let (disk, entries) = compacted(&ops, &[4]);
        assert_eq!(disk.folded.entries, 3);
        let sectors = entries
            .iter()
            .filter(|entry| entry.changes_data())
            .map(|entry| entry.sector)
            .collect::<Vec<_>>();
        assert_eq!(sectors, [4, 12, 4, 8, 16]);
        assert_compaction_changes_nothing(&ops, &[4], &[0, 1]);
    }

    #[test]
    fn compaction_keeps_only_the_writes_still_waiting_for_a_flush() {
        let ops = [
            Op::Write(0, 1, 2),
            Op::Flush,
            Op::Fua(4, 2, 1),
            Op::Mark,
            Op::Fua(8, 3, 2),
            Op::Write(12, 4, 1),
            Op::Fua(16, 5, 1),
        ];
        let (disk, _) = compacted(&ops, &[5]);
        assert_eq!(disk.folded.entries, 5);
        assert!(disk.folded.waiting.is_empty() && disk.folded.held.is_empty());
        let (disk, _) = compacted(&ops, &[5, 7]);
        assert_eq!((disk.folded.entries, disk.folded.flushed), (6, 2));
        assert_eq!(shape(&disk.folded.waiting), [(12, 1, 0)]);
        assert_eq!(disk.folded.held, [4; SECTOR_BYTES]);
        for points in [&[5][..], &[7], &[5, 7], &[3, 5, 7]] {
            assert_compaction_changes_nothing(&ops, points, &[0, 1, 2]);
        }
    }

    fn mark_header(mark: &str) -> [u8; SECTOR_BYTES] {
        let mut header = [0_u8; SECTOR_BYTES];
        header[16..24].copy_from_slice(&MARK.to_le_bytes());
        header[24..32].copy_from_slice(&(mark.len() as u64).to_le_bytes());
        header[32..32 + mark.len()].copy_from_slice(mark.as_bytes());
        header
    }

    #[test]
    fn a_drain_mark_counts_as_logged_once_it_is_the_last_entry() {
        let mut recorder = Recorder::new();
        let status = |recorder: &Recorder| (recorder.count, recorder.next - 1);
        recorder.apply(&Op::Write(0, 1, 1));
        let (entries, last) = status(&recorder);
        assert!(!drain_logged(&mut recorder.log, entries, last).unwrap());
        recorder.entry(4, 1, FUA, &mark_header(DRAIN_MARK));
        recorder.super_block();
        let (entries, last) = status(&recorder);
        assert!(drain_marked(&mut recorder.log, last).unwrap());
        assert!(
            !drain_logged(&mut recorder.log, entries, last).unwrap(),
            "a write whose data ends in the mark's bytes"
        );
        let before = status(&recorder);
        recorder.drain();
        assert!(!drain_logged(&mut recorder.log, before.0, before.1).unwrap());
        let (entries, last) = status(&recorder);
        assert!(drain_logged(&mut recorder.log, entries, last).unwrap());
        recorder.teardown();
        let entries = read_log(&mut recorder.log, 256).unwrap();
        assert_eq!(shape(&entries), [(4, 1, FUA), (0, 1, 0), (0, 0, MARK)]);
        assert_eq!(recorder.count, 4);

        let mut log = Memory::sectors(64);
        let mut header = [0_u8; SECTOR_BYTES];
        header[0..8].copy_from_slice(&LOG_MAGIC.to_le_bytes());
        header[8..16].copy_from_slice(&LOG_VERSION.to_le_bytes());
        header[16..24].copy_from_slice(&1_u64.to_le_bytes());
        header[24..28].copy_from_slice(&4096_u32.to_le_bytes());
        log.write_at(0, &header).unwrap();
        assert!(!drain_logged(&mut log, 0, 7).unwrap());
        for (mark, logged) in [(FORMAT_MARK, false), (DRAIN_MARK, true)] {
            log.write_at(8, &mark_header(mark)).unwrap();
            assert_eq!(drain_logged(&mut log, 1, 15).unwrap(), logged, "{mark}");
        }
        assert!(!drain_logged(&mut log, 1, u64::MAX).unwrap());
    }

    #[test]
    fn compacting_the_log_leaves_power_loss_remount_and_corruption_unchanged() {
        let (histories, length, schedules) = if cfg!(miri) { (1, 12, 1) } else { (48, 30, 12) };
        let choices = [0, 1, 2, 3, 5, 0x9e37_79b9_7f4a_7c15, u64::MAX];
        for seed in 0..histories {
            let ops = random_history(seed, length);
            for point in 0..=ops.len() {
                assert_compaction_changes_nothing(&ops, &[point], &choices);
            }
            assert_compaction_changes_nothing(&ops, &(0..=ops.len()).collect::<Vec<_>>(), &choices);
            let mut draw = Draw(!seed);
            for _ in 0..schedules {
                let mut points = (0..=ops.len())
                    .filter(|_| draw.below(4) == 0)
                    .collect::<Vec<_>>();
                points.push(points.first().copied().unwrap_or(0));
                assert_compaction_changes_nothing(&ops, &points, &choices);
            }
        }
    }
}
