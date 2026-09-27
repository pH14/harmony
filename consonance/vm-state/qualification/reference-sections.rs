// SPDX-License-Identifier: AGPL-3.0-or-later

fn encode_msrs(msrs: &MsrBlock) -> Result<Vec<u8>, VmStateError> {
    let count = u32::try_from(msrs.0.len()).map_err(|_| VmStateError::InvalidField)?;
    let mut payload = Vec::with_capacity(4 + msrs.0.len() * 12);
    payload.extend_from_slice(&count.to_le_bytes());
    for (&index, &value) in &msrs.0 {
        let pair = MsrPairWire {
            index: index.into(),
            value: value.into(),
        };
        payload.extend_from_slice(pair.as_bytes());
    }
    Ok(payload)
}

pub(crate) fn encode_timers(timers: &TimerQueueState) -> Result<Vec<u8>, VmStateError> {
    let count = u32::try_from(timers.entries.len()).map_err(|_| VmStateError::InvalidField)?;
    validate_timers(&timers.entries, timers.next_seq)?;

    let mut payload = Vec::with_capacity(12 + timers.entries.len() * 32);
    payload.extend_from_slice(&timers.next_seq.to_le_bytes());
    payload.extend_from_slice(&count.to_le_bytes());
    for e in &timers.entries {
        let w = TimerEntryWire {
            deadline_vns: e.deadline_vns.into(),
            seq: e.seq.into(),
            token: e.token.into(),
            period_vns: e.period_vns.into(),
        };
        payload.extend_from_slice(w.as_bytes());
    }
    Ok(payload)
}
