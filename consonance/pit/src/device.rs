// SPDX-License-Identifier: AGPL-3.0-or-later
//! The [`Pit`] state machine: three i8254 counters, the mode/command register,
//! counter-0 → IRQ0 generation, and the V-time-driven countdown.
//!
//! All timer arithmetic is integer-only, computed in `u128` intermediates and
//! saturating to `u64::MAX` — `vtime`'s house style. There is no floating point,
//! no map iteration reaching an output, and no clock read: the caller always
//! supplies `now_vns`.

use crate::error::PitError;
use crate::state::{
    PIT_FREQ_HZ, PIT_PORT_COMMAND, PIT_PORT_COUNTER0, PIT_STATE_VERSION, PitCounterState, PitState,
};

/// Nanoseconds per second — the V-time/tick scaling constant.
const NS_PER_SEC: u128 = 1_000_000_000;

/// Binary counting modulus: the 16-bit counting element wraps at 65536.
const BIN_MODULUS: u32 = 65_536;
/// BCD counting modulus: four decimal digits wrap at 10000.
const BCD_MODULUS: u32 = 10_000;

/// Configuration for constructing a [`Pit`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PitConfig {
    /// The i8254 input frequency in Hz — the contract's [`PIT_FREQ_HZ`]
    /// (1.193182 MHz). The counters decrement at this rate of V-time. Must be
    /// non-zero (the countdown arithmetic divides by it).
    pub freq_hz: u64,
}

impl Default for PitConfig {
    /// The contract frequency, [`PIT_FREQ_HZ`].
    fn default() -> Self {
        PitConfig {
            freq_hz: PIT_FREQ_HZ,
        }
    }
}

/// One i8254 counter: its programming (mode/access/BCD), the loaded reload value +
/// V-time arm anchor, and the read/write flip-flop + latch bookkeeping.
#[derive(Clone, Copy, Debug, Default)]
struct Counter {
    /// Mode field `M` as programmed (`0..=7`; 6/7 alias 2/3 in behaviour).
    mode: u8,
    /// BCD (`true`) vs 16-bit binary (`false`).
    bcd: bool,
    /// Access mode (`RW`): 1 = lobyte, 2 = hibyte, 3 = lobyte/hibyte.
    access: u8,
    /// Raw reload register (`0` = full scale).
    reload: u16,
    /// V-time the counter was last (re)armed.
    arm_vns: u64,
    /// A valid count is loaded and counting.
    loaded: bool,
    /// A one-shot mode reached terminal count (no further IRQ until reprogrammed).
    oneshot_fired: bool,
    /// Status NULL-count bit: control word written, count not yet loaded.
    null_count: bool,
    /// Write flip-flop (lobyte/hibyte): false = expect low byte, true = high.
    write_phase: bool,
    /// Low byte held mid-write (lobyte/hibyte access).
    write_lo: u8,
    /// Read flip-flop (lobyte/hibyte): false = low byte next, true = high.
    read_phase: bool,
    /// A counter-latch command has latched [`Self::latch_val`].
    count_latched: bool,
    /// Latched count (raw register encoding).
    latch_val: u16,
    /// A read-back command has latched [`Self::status_val`].
    status_latched: bool,
    /// Latched status byte.
    status_val: u8,
}

/// A userspace-emulated i8254 PIT: three counters + the command register.
///
/// Construct with [`Pit::new`]; drive it with [`Pit::port_write`] / [`Pit::port_read`]
/// (the guest's `0x40`–`0x43` accesses), [`Pit::advance_to`] (the V-time tick),
/// [`Pit::next_irq0_deadline`] (the clock-event deadline `vmm-core` sources), and
/// [`Pit::irq0_pending`] / [`Pit::ack_irq0`] (the IRQ0 edge). Snapshot/restore via
/// [`Pit::snapshot`] / [`Pit::restore`]. It never reads a clock — every
/// time-dependent value is a pure function of the `now_vns` the caller passes.
#[derive(Clone, Debug)]
pub struct Pit {
    /// Frozen input frequency in Hz; invariant: non-zero.
    freq_hz: u64,
    /// Counters 0, 1, 2. Only counter 0 raises IRQ0.
    counters: [Counter; 3],
    /// Counter 0 raised an IRQ0 edge the interrupt controller has not yet
    /// acknowledged. Set by [`Pit::advance_to`], cleared by [`Pit::ack_irq0`].
    irq0_pending: bool,
}

impl Pit {
    /// A fresh PIT: all three counters unloaded (no count programmed), no pending
    /// IRQ0. The guest programs the counters before any tick.
    ///
    /// # Errors
    ///
    /// [`PitError::ZeroFrequency`] if `cfg.freq_hz == 0`.
    pub fn new(cfg: PitConfig) -> Result<Pit, PitError> {
        if cfg.freq_hz == 0 {
            return Err(PitError::ZeroFrequency);
        }
        Ok(Pit {
            freq_hz: cfg.freq_hz,
            counters: [Counter::default(); 3],
            irq0_pending: false,
        })
    }

    /// Service a guest port **write** (`OUT`) to a PIT register. `0x43` is the
    /// command register (control word / latch / read-back); `0x40`–`0x42` load the
    /// selected counter. `now_vns` anchors a count load. Only the low 8 bits of
    /// `value` are meaningful (PIT registers are byte-wide).
    ///
    /// # Errors
    ///
    /// [`PitError::BadPort`] if `port` is not `0x40..=0x43`.
    pub fn port_write(&mut self, port: u16, value: u8, now_vns: u64) -> Result<(), PitError> {
        match port {
            PIT_PORT_COMMAND => {
                self.command_write(value, now_vns);
                Ok(())
            }
            PIT_PORT_COUNTER0..=PIT_PORT_COUNTER0_2 => {
                let i = (port - PIT_PORT_COUNTER0) as usize;
                self.counters[i].counter_write(value, now_vns);
                Ok(())
            }
            _ => Err(PitError::BadPort(port)),
        }
    }

    /// Service a guest port **read** (`IN`) from a PIT register. A counter port
    /// returns the next byte of the latched-or-live count (or a latched status
    /// byte). The command register `0x43` is write-only and reads `0`.
    ///
    /// # Errors
    ///
    /// [`PitError::BadPort`] if `port` is not `0x40..=0x43`.
    pub fn port_read(&mut self, port: u16, now_vns: u64) -> Result<u8, PitError> {
        match port {
            PIT_PORT_COMMAND => Ok(0), // write-only command register
            PIT_PORT_COUNTER0..=PIT_PORT_COUNTER0_2 => {
                let i = (port - PIT_PORT_COUNTER0) as usize;
                let freq = self.freq_hz;
                Ok(self.counters[i].counter_read(now_vns, freq))
            }
            _ => Err(PitError::BadPort(port)),
        }
    }

    /// Advance V-time to `now_vns`. If counter 0's terminal count is now due, raise
    /// IRQ0 ([`Pit::irq0_pending`] becomes `true`) and, for a periodic mode (2/3),
    /// re-arm for the next period (closed-form, catching up missed periods so a
    /// `now_vns` that jumps many periods ahead lands exactly). Idempotent for a
    /// given `now_vns`. Returns `true` if IRQ0 was newly raised this call.
    ///
    /// Only counter 0 is advanced — it is the sole interrupt source; counters 1/2
    /// read back from their own `arm_vns` with no advance needed (their current
    /// count is a modular function of `now_vns`).
    pub fn advance_to(&mut self, now_vns: u64) -> bool {
        let freq = self.freq_hz;
        if self.counters[0].advance(now_vns, freq) {
            self.irq0_pending = true;
            true
        } else {
            false
        }
    }

    /// Absolute V-time (ns) at which counter 0 next raises IRQ0, or `None` if
    /// counter 0 is not an interrupt source right now (unloaded, a fired one-shot,
    /// or a GATE-triggered mode 1/5), **or the deadline is unrepresentable in
    /// `u64`** (a reload beyond ~584 000 years of V-time at this frequency — which
    /// cannot actually occur, the reload caps at 65536, but the saturating contract
    /// is kept uniform with `lapic`). This is the clock-event deadline `vmm-core`
    /// sources `preemption_deadline()` / the idle-resume target from.
    pub fn next_irq0_deadline(&self) -> Option<u64> {
        self.counters[0].next_expiry(self.freq_hz)
    }

    /// Whether counter 0 has raised an IRQ0 edge not yet acknowledged. `vmm-core`
    /// gates injection of the timer vector on this (plus the 8259 IRQ0 mask).
    pub fn irq0_pending(&self) -> bool {
        self.irq0_pending
    }

    /// Acknowledge the pending IRQ0 edge — called once the interrupt controller has
    /// accepted the timer vector (the i8254 output pulse has been consumed). A
    /// later terminal count raises it again. No-op (not an error) if none pending.
    pub fn ack_irq0(&mut self) {
        self.irq0_pending = false;
    }

    /// Plain-data snapshot of the whole device, for `vm-state`. Deterministic:
    /// equal [`Pit`] states produce equal [`PitState`].
    pub fn snapshot(&self) -> PitState {
        PitState {
            version: PIT_STATE_VERSION,
            freq_hz: self.freq_hz,
            irq0_pending: self.irq0_pending,
            counters: self.counters.map(|c| c.snapshot()),
        }
    }

    /// Reconstruct a [`Pit`] from a snapshot, observationally identical to the one
    /// that produced it. Absolute V-time deadlines are derived, so they survive
    /// restore unchanged.
    ///
    /// # Errors
    ///
    /// [`PitError::InvalidState`] if the snapshot `version` is not current,
    /// `freq_hz == 0`, or any counter's `access`/`mode` is not bit-reachable
    /// through the programming path (`access ∈ {1,2,3}`, `mode ≤ 7`) — the same
    /// validation boundary `lapic::Lapic::restore` applies.
    pub fn restore(state: &PitState) -> Result<Pit, PitError> {
        if state.version != PIT_STATE_VERSION || state.freq_hz == 0 {
            return Err(PitError::InvalidState);
        }
        let mut counters = [Counter::default(); 3];
        for (slot, cs) in counters.iter_mut().zip(state.counters.iter()) {
            *slot = Counter::restore(cs)?;
        }
        Ok(Pit {
            freq_hz: state.freq_hz,
            counters,
            irq0_pending: state.irq0_pending,
        })
    }

    /// Service a write to the command register (`0x43`).
    fn command_write(&mut self, value: u8, now_vns: u64) {
        let sc = (value >> 6) & 0b11;
        if sc == 0b11 {
            self.readback_command(value, now_vns);
            return;
        }
        let i = sc as usize;
        let rw = (value >> 4) & 0b11;
        let freq = self.freq_hz;
        if rw == 0b00 {
            // Counter-latch command: latch the current count for the next read(s).
            self.counters[i].latch_count(now_vns, freq);
        } else {
            // Control word: (re)program mode/access/BCD; awaits a count write.
            let mode = (value >> 1) & 0b111;
            let bcd = value & 1 != 0;
            self.counters[i].program(rw, mode, bcd);
        }
    }

    /// Service a read-back command (`0x43` with the select field `0b11`): latch the
    /// count and/or status of each selected counter. Bit 5 clear ⇒ latch count;
    /// bit 4 clear ⇒ latch status; bits 1/2/3 select counters 0/1/2.
    fn readback_command(&mut self, value: u8, now_vns: u64) {
        let latch_count = value & (1 << 5) == 0;
        let latch_status = value & (1 << 4) == 0;
        let freq = self.freq_hz;
        for i in 0..3 {
            if value & (1 << (i + 1)) != 0 {
                // Status is latched first so a combined read returns it before the
                // count (the counter-read path checks `status_latched` first).
                if latch_status {
                    self.counters[i].latch_status(now_vns, freq);
                }
                if latch_count {
                    self.counters[i].latch_count(now_vns, freq);
                }
            }
        }
    }
}

/// One past counter 2's port — `0x42`. A named bound so the `0x40..=0x42` range
/// patterns read clearly (`PIT_PORT_COUNTER2` is the same value).
const PIT_PORT_COUNTER0_2: u16 = PIT_PORT_COUNTER0 + 2;

impl Counter {
    /// (Re)program from a control word: set access/mode/BCD, clear the loaded count
    /// (a count write must follow), and reset both flip-flops and any latch — the
    /// i8254 resets the read/write flip-flop on a control-word write.
    fn program(&mut self, rw: u8, mode: u8, bcd: bool) {
        self.access = rw;
        self.mode = mode;
        self.bcd = bcd;
        self.loaded = false;
        self.oneshot_fired = false;
        self.null_count = true;
        self.write_phase = false;
        self.read_phase = false;
        self.count_latched = false;
        self.status_latched = false;
    }

    /// Service a write to this counter's port (`0x40`–`0x42`): assemble the reload
    /// value per the access mode and (re)arm the countdown at `now`.
    fn counter_write(&mut self, value: u8, now: u64) {
        match self.access {
            1 => {
                // lobyte only: high byte is zero.
                self.reload = u16::from(value);
                self.load(now);
            }
            2 => {
                // hibyte only: low byte is zero.
                self.reload = u16::from(value) << 8;
                self.load(now);
            }
            3 => {
                if !self.write_phase {
                    // Low byte: held; the counter is not (re)armed until the high
                    // byte arrives (the count is incomplete — NULL count set).
                    self.write_lo = value;
                    self.write_phase = true;
                    self.null_count = true;
                } else {
                    self.reload = (u16::from(value) << 8) | u16::from(self.write_lo);
                    self.write_phase = false;
                    self.load(now);
                }
            }
            // Access 0 is the latch command (never stored as an access mode); any
            // other value is unreachable through the programming path. Drop.
            _ => {}
        }
    }

    /// Complete a count load: anchor the countdown at `now` and clear the
    /// one-shot-fired / NULL-count state.
    fn load(&mut self, now: u64) {
        self.arm_vns = now;
        self.loaded = true;
        self.oneshot_fired = false;
        self.null_count = false;
    }

    /// Service a read of this counter's port. A latched status byte is returned
    /// first (read-back), then the latched-or-live count per the access mode + read
    /// flip-flop.
    fn counter_read(&mut self, now: u64, freq: u64) -> u8 {
        if self.status_latched {
            self.status_latched = false;
            return self.status_val;
        }
        let val = if self.count_latched {
            self.latch_val
        } else {
            self.current_raw(now, freq)
        };
        match self.access {
            1 => {
                self.count_latched = false;
                (val & 0xFF) as u8
            }
            2 => {
                self.count_latched = false;
                (val >> 8) as u8
            }
            3 => {
                if !self.read_phase {
                    self.read_phase = true;
                    (val & 0xFF) as u8
                } else {
                    self.read_phase = false;
                    self.count_latched = false;
                    (val >> 8) as u8
                }
            }
            _ => 0,
        }
    }

    /// Latch the current count for the next read(s). A latch command while a latch
    /// is already pending is ignored (the first latched value is preserved).
    fn latch_count(&mut self, now: u64, freq: u64) {
        if !self.count_latched {
            self.latch_val = self.current_raw(now, freq);
            self.count_latched = true;
        }
    }

    /// Latch the status byte for the next read (read-back command). Ignored if one
    /// is already pending.
    fn latch_status(&mut self, now: u64, freq: u64) {
        if !self.status_latched {
            self.status_val = self.status_byte(now, freq);
            self.status_latched = true;
        }
    }

    /// The decoded mode (`6`→2, `7`→3; otherwise the raw value).
    fn decoded_mode(&self) -> u8 {
        match self.mode & 0b111 {
            6 => 2,
            7 => 3,
            m => m,
        }
    }

    /// Is this a periodic mode (rate generator / square wave)?
    fn periodic(&self) -> bool {
        matches!(self.decoded_mode(), 2 | 3)
    }

    /// Does this mode generate an IRQ on counter 0 (gate tied high)? Modes 0/4 are
    /// one-shot, 2/3 periodic; modes 1/5 are GATE-triggered and never fire with the
    /// gate tied high.
    fn irq_generating(&self) -> bool {
        matches!(self.decoded_mode(), 0 | 2 | 3 | 4)
    }

    /// The counting modulus for this counter (binary vs BCD).
    fn modulus(&self) -> u32 {
        if self.bcd { BCD_MODULUS } else { BIN_MODULUS }
    }

    /// The decoded reload count `N` (the number of input ticks per period): the
    /// register value, with `0` meaning full scale (65536 binary / 10000 BCD).
    fn decoded_reload(&self) -> u32 {
        decode_count(self.reload, self.bcd)
    }

    /// V-time to count down `n` ticks, **un-saturated**: `ceil(n · 1e9 / freq)` in
    /// `u128`. **Ceil** so the counter never fires before `n` whole ticks elapse.
    /// `freq` is non-zero by construction, and `n ≤ 65536`, so the product cannot
    /// overflow `u128` and the division never traps.
    fn period_ns(n: u32, freq: u64) -> u128 {
        (u128::from(n) * NS_PER_SEC).div_ceil(u128::from(freq))
    }

    /// Whole ticks elapsed over `delta` V-time: `floor(delta · freq / 1e9)`,
    /// saturating to `u64::MAX`.
    fn ticks(delta: u64, freq: u64) -> u64 {
        sat_u64((u128::from(delta) * u128::from(freq)) / NS_PER_SEC)
    }

    /// The next absolute V-time at which this counter raises IRQ0, or `None` if it
    /// is not an interrupt source (unloaded / fired one-shot / GATE-triggered) or
    /// the deadline is unrepresentable in `u64`.
    fn next_expiry(&self, freq: u64) -> Option<u64> {
        if !self.loaded || !self.irq_generating() {
            return None;
        }
        if !self.periodic() && self.oneshot_fired {
            return None;
        }
        let deadline = u128::from(self.arm_vns) + Self::period_ns(self.decoded_reload(), freq);
        u64::try_from(deadline).ok()
    }

    /// Advance to `now`: if a terminal count is due, fire (return `true`) and, for a
    /// periodic mode, re-arm to the period boundary at-or-before `now` (catching up
    /// missed periods closed-form). A one-shot fires once and is consumed.
    /// Idempotent: a repeat call at the same `now` does not re-fire.
    fn advance(&mut self, now: u64, freq: u64) -> bool {
        if !self.loaded || !self.irq_generating() {
            return false;
        }
        if !self.periodic() && self.oneshot_fired {
            return false;
        }
        let period = Self::period_ns(self.decoded_reload(), freq);
        // Decide on the un-saturated `u128` span (a saturating deadline near
        // `u64::MAX` would clamp and re-fire forever, breaking idempotence).
        let elapsed = u128::from(now.saturating_sub(self.arm_vns));
        if elapsed < period {
            return false;
        }
        if self.periodic() {
            // `period ≥ 1` (n ≥ 1), so `k ≥ 1`. Re-anchor to the last period
            // boundary at-or-before `now`: drift-free, and a repeat call at the same
            // `now` then sees `elapsed < period` and is a no-op (idempotent).
            let k = elapsed / period;
            self.arm_vns = sat_u64(u128::from(self.arm_vns) + k * period);
        } else {
            // One-shot: fire once, then consume so no gating change resurrects it.
            self.oneshot_fired = true;
        }
        true
    }

    /// The current counting-element value (decoded, 0-based), a pure function of
    /// `now` and the arm anchor:
    ///
    /// - **periodic** (modes 2/3): `N − (ticks mod N)`, i.e. the count cycles
    ///   `N..=1` and reloads — it never reads 0 (a rate generator's lowest count is
    ///   1). The mode-3 square wave's by-2 decrement is **not** modelled in the
    ///   read-back (only the IRQ *rate* — `N` ticks per period — is load-bearing and
    ///   is exact); see `IMPLEMENTATION.md`.
    /// - **one-shot / gate modes** (0/1/4/5): `N − ticks`, wrapping modulo the
    ///   counting modulus after terminal count (the counter keeps decrementing past
    ///   0, as the hardware does).
    fn current_dec(&self, now: u64, freq: u64) -> u32 {
        let n = self.decoded_reload();
        let m = u64::from(self.modulus());
        let ticks = Self::ticks(now.saturating_sub(self.arm_vns), freq);
        if self.periodic() {
            // n ≥ 1, so `ticks % n` ∈ 0..n and `n − pos` ∈ 1..=n.
            let pos = ticks % u64::from(n);
            n - pos as u32
        } else {
            // (n − ticks) reduced into 0..modulus (wrapping past terminal count).
            let pos = ticks % m;
            (u64::from(n) + m - pos).rem_euclid(m) as u32
        }
    }

    /// The current count encoded as the raw 16-bit register value (BCD or binary)
    /// — what a counter-port read returns.
    fn current_raw(&self, now: u64, freq: u64) -> u16 {
        if !self.loaded {
            return 0;
        }
        encode_count(self.current_dec(now, freq), self.bcd)
    }

    /// The read-back status byte: `OUTPUT(7) | NULL(6) | RW(5:4) | M(3:1) | BCD(0)`.
    /// The OUTPUT pin (bit 7) is a **best-effort** model of the waveform phase — it
    /// is not load-bearing (the IRQ timing is exact and is what Linux uses); see
    /// `IMPLEMENTATION.md`.
    fn status_byte(&self, now: u64, freq: u64) -> u8 {
        let output = u8::from(self.output_pin(now, freq)) << 7;
        let null = u8::from(self.null_count) << 6;
        let rw = (self.access & 0b11) << 4;
        let m = (self.mode & 0b111) << 1;
        let bcd = u8::from(self.bcd);
        output | null | rw | m | bcd
    }

    /// Best-effort OUTPUT-pin state for the status byte (see [`Self::status_byte`]).
    fn output_pin(&self, now: u64, freq: u64) -> bool {
        if !self.loaded {
            // Unloaded: mode 0 holds OUT low, the others high (the i8254 reset
            // convention closest to a freshly-controlled counter).
            return self.decoded_mode() != 0;
        }
        let n = self.decoded_reload();
        let ticks = Self::ticks(now.saturating_sub(self.arm_vns), freq);
        match self.decoded_mode() {
            // Mode 0: OUT low until terminal count, then high.
            0 => ticks >= u64::from(n),
            // Mode 2: OUT high except the single clock at count 1.
            2 => self.current_dec(now, freq) != 1,
            // Mode 3: square wave — high the first half of each period.
            3 => {
                let pos = ticks % u64::from(n);
                pos < u64::from(n.div_ceil(2))
            }
            // Mode 4: strobe — OUT high except the single clock at terminal count.
            4 => ticks != u64::from(n),
            // Modes 1/5 (gate-triggered, gate tied high): OUT idle high.
            _ => true,
        }
    }

    /// Plain-data snapshot of this counter.
    fn snapshot(&self) -> PitCounterState {
        PitCounterState {
            mode: self.mode,
            bcd: self.bcd,
            access: self.access,
            reload: self.reload,
            arm_vns: self.arm_vns,
            loaded: self.loaded,
            oneshot_fired: self.oneshot_fired,
            null_count: self.null_count,
            write_phase: self.write_phase,
            write_lo: self.write_lo,
            read_phase: self.read_phase,
            count_latched: self.count_latched,
            latch_val: self.latch_val,
            status_latched: self.status_latched,
            status_val: self.status_val,
        }
    }

    /// Reconstruct a counter from its snapshot, validating that the access/mode
    /// fields are bit-reachable through the programming path.
    fn restore(cs: &PitCounterState) -> Result<Counter, PitError> {
        // `access` is 0 only on a never-programmed counter (default), and 1/2/3
        // once programmed; nothing else is reachable. `mode` is a 3-bit field.
        if cs.access > 3 || cs.mode > 7 {
            return Err(PitError::InvalidState);
        }
        // A loaded counter must have a real access mode (the control word set it).
        if cs.loaded && cs.access == 0 {
            return Err(PitError::InvalidState);
        }
        Ok(Counter {
            mode: cs.mode,
            bcd: cs.bcd,
            access: cs.access,
            reload: cs.reload,
            arm_vns: cs.arm_vns,
            loaded: cs.loaded,
            oneshot_fired: cs.oneshot_fired,
            null_count: cs.null_count,
            write_phase: cs.write_phase,
            write_lo: cs.write_lo,
            read_phase: cs.read_phase,
            count_latched: cs.count_latched,
            latch_val: cs.latch_val,
            status_latched: cs.status_latched,
            status_val: cs.status_val,
        })
    }
}

// --- free helpers -----------------------------------------------------------

/// Decode a raw reload register into the binary tick count `N`, with `0` meaning
/// full scale (65536 binary / 10000 BCD).
fn decode_count(raw: u16, bcd: bool) -> u32 {
    if bcd {
        let v = bcd_to_bin(raw);
        if v == 0 { BCD_MODULUS } else { v }
    } else if raw == 0 {
        BIN_MODULUS
    } else {
        u32::from(raw)
    }
}

/// Encode a decoded count back into the raw 16-bit register value (BCD or binary).
/// A value equal to the modulus encodes to `0` (binary `65536 → 0x0000`).
fn encode_count(value: u32, bcd: bool) -> u16 {
    if bcd {
        bin_to_bcd(value % BCD_MODULUS)
    } else {
        // `value` is in 0..=65536; truncation maps 65536 → 0, matching the register.
        value as u16
    }
}

/// Sum the four BCD nibbles positionally. Out-of-range nibbles (`A`–`F`) are summed
/// as their value (no real i8254 input produces them — Linux uses binary mode — but
/// this stays total and deterministic rather than panicking on untrusted input).
fn bcd_to_bin(raw: u16) -> u32 {
    let d = |shift: u32| u32::from((raw >> shift) & 0xF);
    d(12) * 1000 + d(8) * 100 + d(4) * 10 + d(0)
}

/// Encode `value` (0..=9999) as 4-digit BCD.
fn bin_to_bcd(value: u32) -> u16 {
    let v = value % BCD_MODULUS;
    let bcd =
        (((v / 1000) % 10) << 12) | (((v / 100) % 10) << 8) | (((v / 10) % 10) << 4) | (v % 10);
    bcd as u16
}

/// Saturate a `u128` intermediate to `u64` (the crate-wide overflow rule).
fn sat_u64(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// Formal proof harnesses (bounded model checking via Kani); compiled only under
/// `cargo kani`. Declared as a child of `device` so `use super::*` reaches the
/// private helpers it verifies. See `IMPLEMENTATION.md` ("Formal proofs (Kani)").
#[cfg(kani)]
#[path = "device_proofs.rs"]
mod proofs;

#[cfg(test)]
mod tests;
