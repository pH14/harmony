// SPDX-License-Identifier: AGPL-3.0-or-later

impl Arm64VmState {
    pub fn encode(&self) -> Result<Vec<u8>, VmStateError> {
        let section_count = SECTION_COUNT + u16::from(!self.engine_state.is_empty());
        let mut out = Vec::new();
        out.extend_from_slice(
            HeaderWire {
                magic: VM_STATE_MAGIC.into(),
                version: VM_STATE_VERSION.into(),
                arch: ARCH_AARCH64.into(),
                section_count: section_count.into(),
            }
            .as_bytes(),
        );

        put_section(
            &mut out,
            TAG_REGS,
            Arm64RegsWire::from(&self.regs).as_bytes(),
        )?;
        put_section(
            &mut out,
            TAG_SYSREGS,
            Arm64SysregsWire::from(&self.sysregs).as_bytes(),
        )?;
        put_section(&mut out, TAG_MP_STATE, &[encode_mp_state(self.mp_state)])?;
        put_section(&mut out, TAG_VTIME, VtimeWire::from(&self.vtime).as_bytes())?;
        put_section(&mut out, TAG_TIMERS, &encode_timers(&self.timers)?)?;
        put_section(&mut out, TAG_HYPERCALL, &self.hypercall)?;
        put_section(&mut out, TAG_DEVICES, &self.devices.0)?;
        put_section(&mut out, TAG_CONTRACT_HASH, &self.contract_hash)?;
        put_section(
            &mut out,
            TAG_SIMD_FP,
            Arm64SimdFpWire::from(&self.simd_fp).as_bytes(),
        )?;
        put_section(
            &mut out,
            TAG_DEBUG,
            Arm64DebugWire::from(&self.debug).as_bytes(),
        )?;
        put_section(
            &mut out,
            TAG_VTIMER,
            Arm64VtimerWire::from(&self.vtimer).as_bytes(),
        )?;
        put_section(
            &mut out,
            TAG_INTERRUPTS,
            Arm64InterruptsWire::from(&self.interrupts).as_bytes(),
        )?;
        if !self.engine_state.is_empty() {
            put_section(&mut out, TAG_ENGINE_STATE, &self.engine_state)?;
        }

        Ok(out)
    }
}
