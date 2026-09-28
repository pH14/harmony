// SPDX-License-Identifier: AGPL-3.0-or-later

fn encode_sdk_channel(
    sdk: &SdkChannel,
    recorded: Option<&channel::RecordedState>,
) -> Result<Vec<u8>, channel::ChannelError> {
    let mut v = Vec::new();
    let recorded = match recorded {
        Some(state) => state.encode(),
        None => sdk.env.snapshot_state()?.encode(),
    };
    v.extend_from_slice(&(recorded.len() as u64).to_le_bytes());
    v.extend_from_slice(&recorded);
    match &sdk.pending_stop {
        None => v.push(0),
        Some(SdkStop::Assertion { id, data }) => {
            v.push(1);
            v.extend_from_slice(&id.to_le_bytes());
            v.extend_from_slice(&(data.len() as u32).to_le_bytes());
            v.extend_from_slice(data);
        }
        Some(SdkStop::Quiescent) => v.push(2),
        Some(SdkStop::Decision {
            moment,
            seq,
            question,
        }) => {
            v.push(3);
            v.extend_from_slice(&moment.to_le_bytes());
            v.extend_from_slice(&seq.to_le_bytes());
            v.extend_from_slice(&question.service().to_le_bytes());
            v.extend_from_slice(&question.request_id().to_le_bytes());
            v.extend_from_slice(&(question.payload().len() as u32).to_le_bytes());
            v.extend_from_slice(question.payload());
        }
    }
    v.push(u8::from(sdk.pending_snapshot));
    if !sdk.coverage_thresholds.is_empty() {
        v.extend_from_slice(b"COVR");
        let count = u64::try_from(sdk.coverage_thresholds.len()).unwrap_or(u64::MAX);
        v.extend_from_slice(&count.to_le_bytes());
        for (thread, threshold) in &sdk.coverage_thresholds {
            v.extend_from_slice(&thread.to_le_bytes());
            v.extend_from_slice(&threshold.to_le_bytes());
        }
    }
    Ok(v)
}
