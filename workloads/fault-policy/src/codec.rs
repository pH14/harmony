// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::catalog::{Answer, Fault};
use crate::error::EnvError;
use crate::host::{Action, BitMask, HostFault, Ratio};
use crate::{MAX_SUPPLY_LEN, Span};

const ANS_NOMINAL: u8 = 0;
const ANS_SUPPLY: u8 = 1;
const ANS_FAULT: u8 = 2;

const ACT_HOST: u8 = 0;
const ACT_GUEST: u8 = 1;

const HF_SKEW_TIME: u8 = 0;
const HF_SET_CLOCK_RATE: u8 = 1;
const HF_CORRUPT_MEMORY: u8 = 2;
const HF_INJECT_INTERRUPT: u8 = 3;

const F_BLOCK_EIO: u8 = 5;
const F_BLOCK_LATENCY: u8 = 6;
const F_BLOCK_TORN: u8 = 7;
const F_BLOCK_NOSPC: u8 = 8;
const F_PROC_PAUSE: u8 = 9;
const F_PROC_KILL: u8 = 10;
const F_PROC_RESTART: u8 = 11;
const F_NET_LATENCY: u8 = 12;
const F_NET_LOSS: u8 = 13;
const F_NET_THROTTLE: u8 = 14;
const F_NET_RESET: u8 = 15;
const F_BUGGIFY_FIRE: u8 = 16;
const F_RUN_HOOK: u8 = 17;
const F_PROC_PARK: u8 = 19;

pub(crate) fn put_u16(w: &mut Vec<u8>, v: u16) {
    w.extend_from_slice(&v.to_le_bytes());
}

pub(crate) fn put_u32(w: &mut Vec<u8>, v: u32) {
    w.extend_from_slice(&v.to_le_bytes());
}

pub(crate) fn put_u64(w: &mut Vec<u8>, v: u64) {
    w.extend_from_slice(&v.to_le_bytes());
}

pub(crate) fn put_len(w: &mut Vec<u8>, n: usize) {
    put_u32(w, u32::try_from(n).unwrap_or(u32::MAX));
}

pub(crate) fn put_bytes(w: &mut Vec<u8>, b: &[u8]) {
    put_len(w, b.len());
    w.extend_from_slice(b);
}

pub(crate) fn write_fault(w: &mut Vec<u8>, f: &Fault) {
    match f {
        Fault::NetLatency(Span(d)) => {
            w.push(F_NET_LATENCY);
            put_u64(w, *d);
        }
        Fault::NetLoss { num, den } => {
            w.push(F_NET_LOSS);
            put_u16(w, *num);
            put_u16(w, *den);
        }
        Fault::NetThrottle { bps } => {
            w.push(F_NET_THROTTLE);
            put_u32(w, *bps);
        }
        Fault::NetReset => w.push(F_NET_RESET),
        Fault::BlockEio => w.push(F_BLOCK_EIO),
        Fault::BlockLatency(Span(d)) => {
            w.push(F_BLOCK_LATENCY);
            put_u64(w, *d);
        }
        Fault::BlockTorn(n) => {
            w.push(F_BLOCK_TORN);
            put_u32(w, *n);
        }
        Fault::BlockNospc => w.push(F_BLOCK_NOSPC),
        Fault::ProcPause(Span(d)) => {
            w.push(F_PROC_PAUSE);
            put_u64(w, *d);
        }
        Fault::ProcKill => w.push(F_PROC_KILL),
        Fault::ProcRestart => w.push(F_PROC_RESTART),
        Fault::BuggifyFire => w.push(F_BUGGIFY_FIRE),
        Fault::RunHook(id) => {
            w.push(F_RUN_HOOK);
            put_u32(w, *id);
        }
        Fault::ProcPark { addr, hits, hold } => {
            w.push(F_PROC_PARK);
            put_u64(w, *addr);
            put_u32(w, *hits);
            put_u64(w, hold.0);
        }
    }
}

pub(crate) fn read_fault(r: &mut Reader) -> Result<Fault, EnvError> {
    let f = match r.u8()? {
        F_NET_LATENCY => Fault::NetLatency(Span(r.u64()?)),
        F_NET_LOSS => Fault::NetLoss {
            num: r.u16()?,
            den: r.u16()?,
        },
        F_NET_THROTTLE => Fault::NetThrottle { bps: r.u32()? },
        F_NET_RESET => Fault::NetReset,
        F_BLOCK_EIO => Fault::BlockEio,
        F_BLOCK_LATENCY => Fault::BlockLatency(Span(r.u64()?)),
        F_BLOCK_TORN => Fault::BlockTorn(r.u32()?),
        F_BLOCK_NOSPC => Fault::BlockNospc,
        F_PROC_PAUSE => Fault::ProcPause(Span(r.u64()?)),
        F_PROC_KILL => Fault::ProcKill,
        F_PROC_RESTART => Fault::ProcRestart,
        F_BUGGIFY_FIRE => Fault::BuggifyFire,
        F_RUN_HOOK => Fault::RunHook(r.u32()?),
        F_PROC_PARK => Fault::ProcPark {
            addr: r.u64()?,
            hits: r.u32()?,
            hold: Span(r.u64()?),
        },
        _ => return Err(EnvError::Malformed),
    };
    Ok(f)
}

pub(crate) fn write_answer(w: &mut Vec<u8>, a: &Answer) {
    match a {
        Answer::Nominal => w.push(ANS_NOMINAL),
        Answer::Supply(v) => {
            w.push(ANS_SUPPLY);
            put_bytes(w, v);
        }
        Answer::Fault(f) => {
            w.push(ANS_FAULT);
            write_fault(w, f);
        }
    }
}

pub(crate) fn read_answer(r: &mut Reader) -> Result<Answer, EnvError> {
    let a = match r.u8()? {
        ANS_NOMINAL => Answer::Nominal,
        ANS_SUPPLY => {
            let b = r.bytes()?;
            if b.len() > MAX_SUPPLY_LEN as usize {
                return Err(EnvError::Malformed);
            }
            Answer::Supply(b.to_vec())
        }
        ANS_FAULT => Answer::Fault(read_fault(r)?),
        _ => return Err(EnvError::Malformed),
    };
    Ok(a)
}

pub(crate) fn write_host_fault(w: &mut Vec<u8>, f: &HostFault) {
    match f {
        HostFault::SkewTime(Span(d)) => {
            w.push(HF_SKEW_TIME);
            put_u64(w, *d);
        }
        HostFault::SetClockRate(r) => {
            w.push(HF_SET_CLOCK_RATE);
            put_u64(w, r.num());
            put_u64(w, r.den());
        }
        HostFault::CorruptMemory {
            gpa,
            mask: BitMask(mask),
        } => {
            w.push(HF_CORRUPT_MEMORY);
            put_u64(w, *gpa);
            put_u64(w, *mask);
        }
        HostFault::InjectInterrupt { vector } => {
            w.push(HF_INJECT_INTERRUPT);
            put_u32(w, *vector);
        }
    }
}

pub(crate) fn read_host_fault(r: &mut Reader) -> Result<HostFault, EnvError> {
    let f = match r.u8()? {
        HF_SKEW_TIME => HostFault::SkewTime(Span(r.u64()?)),
        HF_SET_CLOCK_RATE => {
            let num = r.u64()?;
            let den = r.u64()?;
            HostFault::SetClockRate(Ratio::new(num, den).ok_or(EnvError::Malformed)?)
        }
        HF_CORRUPT_MEMORY => HostFault::CorruptMemory {
            gpa: r.u64()?,
            mask: BitMask(r.u64()?),
        },
        HF_INJECT_INTERRUPT => HostFault::InjectInterrupt { vector: r.u32()? },
        _ => return Err(EnvError::Malformed),
    };
    Ok(f)
}

pub(crate) fn write_action(w: &mut Vec<u8>, a: &Action) {
    match a {
        Action::Host(f) => {
            w.push(ACT_HOST);
            write_host_fault(w, f);
        }
        Action::Guest(ans) => {
            w.push(ACT_GUEST);
            write_answer(w, ans);
        }
    }
}

pub(crate) fn read_action(r: &mut Reader) -> Result<Action, EnvError> {
    let a = match r.u8()? {
        ACT_HOST => Action::Host(read_host_fault(r)?),
        ACT_GUEST => Action::Guest(read_answer(r)?),
        _ => return Err(EnvError::Malformed),
    };
    Ok(a)
}

pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub(crate) fn at_end(&self) -> bool {
        self.pos == self.buf.len()
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], EnvError> {
        let end = self.pos.checked_add(n).ok_or(EnvError::Malformed)?;
        let slice = self.buf.get(self.pos..end).ok_or(EnvError::Malformed)?;
        self.pos = end;
        Ok(slice)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, EnvError> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16, EnvError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub(crate) fn u32(&mut self) -> Result<u32, EnvError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub(crate) fn u64(&mut self) -> Result<u64, EnvError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    pub(crate) fn bytes(&mut self) -> Result<&'a [u8], EnvError> {
        let len = self.u32()? as usize;
        self.take(len)
    }
}
