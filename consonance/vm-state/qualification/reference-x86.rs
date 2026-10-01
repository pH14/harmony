// SPDX-License-Identifier: AGPL-3.0-or-later

impl VmState {
    pub fn encode(&self) -> Result<Vec<u8>, VmStateError> {
        let section_count = SECTION_COUNT
            + u16::from(!self.engine_state.is_empty())
            + u16::from(self.xsave_restore_bv.is_some())
            + u16::from(self.nested_state.is_some());
        if let Some(bytes) = &self.nested_state {
            validate_nested_shape(bytes)?;
        }
        let mut out = Vec::new();
        out.extend_from_slice(
            HeaderWire {
                magic: VM_STATE_MAGIC.into(),
                version: VM_STATE_VERSION.into(),
                arch: ARCH_X86_64.into(),
                section_count: section_count.into(),
            }
            .as_bytes(),
        );

        put_section(&mut out, TAG_REGS, RegsWire::from(&self.regs).as_bytes())?;
        put_section(&mut out, TAG_SREGS, SregsWire::from(&self.sregs).as_bytes())?;
        put_section(&mut out, TAG_XCRS, XcrsWire::from(&self.xcrs).as_bytes())?;
        put_section(
            &mut out,
            TAG_DEBUGREGS,
            DebugRegsWire::from(&self.debugregs).as_bytes(),
        )?;
        put_section(
            &mut out,
            TAG_EVENTS,
            EventsWire::from(&self.events).as_bytes(),
        )?;
        put_section(&mut out, TAG_MP_STATE, &[encode_mp_state(self.mp_state)])?;
        put_section(&mut out, TAG_MSRS, &encode_msrs(&self.msrs)?)?;
        put_section(&mut out, TAG_XSAVE, &self.xsave.0)?;
        put_section(&mut out, TAG_VTIME, VtimeWire::from(&self.vtime).as_bytes())?;
        put_section(&mut out, TAG_TIMERS, &encode_timers(&self.timers)?)?;
        put_section(&mut out, TAG_HYPERCALL, &self.hypercall)?;
        put_section(&mut out, TAG_DEVICES, &self.devices.0)?;
        put_section(&mut out, TAG_CONTRACT_HASH, &self.contract_hash)?;
        if !self.engine_state.is_empty() {
            put_section(&mut out, TAG_ENGINE_STATE, &self.engine_state)?;
        }
        if let Some(value) = self.xsave_restore_bv {
            put_section(
                &mut out,
                TAG_XSAVE_RESTORE_BV,
                XsaveRestoreBvWire::from(value).as_bytes(),
            )?;
        }
        if let Some(bytes) = &self.nested_state {
            put_section(&mut out, TAG_NESTED_STATE, bytes)?;
        }

        Ok(out)
    }
}
