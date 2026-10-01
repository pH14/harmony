// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{
    admission::AdmittedModule,
    meter::Meter,
    runtime::{Capture, Continuation, Invocation, Result, Scalar, error},
    services::{Host, Pending, range},
};
use consonance_client::session::{SharedState, SparseSnapshot};
use environment::{
    channel::RecordedState,
    input_spec::{InputSpec, ServiceFactory},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc};
const LIMIT: usize = 16 * 1024 * 1024;
const SIDECAR_LIMIT: usize = 4 * 1024 * 1024;
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct Blob {
    len: u64,
    digest: [u8; 32],
}
impl Blob {
    fn new(bytes: &SharedState) -> Self {
        Self {
            len: bytes.len() as u64,
            digest: bytes.digest(),
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct Body {
    version: u32,
    execution: [u8; 32],
    memory: [u8; 32],
    input: [u8; 32],
    globals: BTreeMap<String, Scalar>,
    tables: BTreeMap<String, Vec<Option<u32>>>,
    fuel: u64,
    continuation: Option<Continuation>,
    data: Vec<usize>,
    elements: Vec<u32>,
    invocation: Invocation,
    started: bool,
    finished: bool,
    trap: Option<String>,
    outputs: Vec<Scalar>,
    pending: Option<Pending>,
    observations: BTreeMap<u32, (u64, u32)>,
    thresholds: BTreeMap<u32, u64>,
    closed: [bool; 2],
    imports: u64,
    meter: Meter,
    blobs: [Blob; 4],
}
#[derive(Clone, Debug)]
pub(crate) struct Sections(pub(crate) [SharedState; 4]);
impl Sections {
    pub(crate) fn capture(
        capture: &Capture,
        input: &InputSpec,
        base: Option<&Self>,
    ) -> Result<Self> {
        let input = SharedState::from_bytes(input.encode(), base.map(|base| &base.0[0]));
        let env = SharedState::from_bytes(
            capture.host.env.snapshot_state().map_err(error)?.encode(),
            base.map(|base| &base.0[1]),
        );
        let sections = Self([
            input,
            env,
            capture.host.events.clone(),
            capture.host.console.clone(),
        ]);
        if sections.0.iter().any(|bytes| bytes.len() > LIMIT) {
            return Err(error("snapshot host-state section exceeds artifact bounds"));
        }
        Ok(sections)
    }
}
impl Body {
    pub(crate) fn capture(
        module: &AdmittedModule,
        capture: &Capture,
        meter: &Meter,
        sections: &Sections,
    ) -> Result<Self> {
        meter.validate(capture.fuel)?;
        Ok(Self {
            version: 1,
            execution: module.execution_digest(),
            memory: *blake3::hash(&capture.memory).as_bytes(),
            input: module.identity_with_input(&sections.0[0].materialize()),
            globals: capture.globals.clone(),
            tables: capture.tables.clone(),
            fuel: capture.fuel,
            continuation: capture.continuation.clone(),
            data: capture.data.clone(),
            elements: capture.elements.clone(),
            invocation: capture.invocation.clone(),
            started: capture.started,
            finished: capture.finished,
            trap: capture.trap.clone(),
            outputs: capture.outputs.clone(),
            pending: capture.host.pending.clone(),
            observations: capture.host.observations.clone(),
            thresholds: capture.host.thresholds.clone(),
            closed: capture.host.closed,
            imports: capture.host.imports,
            meter: meter.clone(),
            blobs: std::array::from_fn(|index| Blob::new(&sections.0[index])),
        })
    }
    pub(crate) fn verify_memory(&self, memory: &[u8]) -> Result<()> {
        if *blake3::hash(memory).as_bytes() != self.memory {
            return Err(error("snapshot memory checksum differs"));
        }
        Ok(())
    }
    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        let bytes = postcard::to_allocvec(self).map_err(error)?;
        if bytes.len() > SIDECAR_LIMIT - 44 {
            return Err(error("snapshot sidecar exceeds artifact bounds"));
        }
        let mut envelope = b"HWAS0001".to_vec();
        envelope.extend((bytes.len() as u32).to_le_bytes());
        envelope.extend(&bytes);
        envelope.extend(Sha256::digest(&envelope));
        Ok(envelope)
    }
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 44 || bytes.len() > SIDECAR_LIMIT || &bytes[..8] != b"HWAS0001" {
            return Err(error("invalid portable sidecar header"));
        }
        let len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        if len != bytes.len() - 44
            || Sha256::digest(&bytes[..bytes.len() - 32]).as_slice() != &bytes[bytes.len() - 32..]
        {
            return Err(error("portable sidecar checksum differs"));
        }
        let body: Self = postcard::from_bytes(&bytes[12..12 + len]).map_err(error)?;
        if body.encode()? != bytes {
            return Err(error("portable sidecar encoding is noncanonical"));
        }
        Ok(body)
    }
}
pub(crate) fn pages(memory: &[u8], sections: &Sections) -> Vec<(u64, Arc<[u8; 4096]>)> {
    let mut pages = Vec::new();
    for (index, page) in memory.chunks_exact(4096).enumerate() {
        if page.iter().any(|byte| *byte != 0) {
            pages.push((index as u64, Arc::new(page.try_into().unwrap())));
        }
    }
    for (index, section) in sections.0.iter().enumerate() {
        let bytes = section.materialize();
        for (page, bytes) in bytes.chunks(4096).enumerate() {
            let mut content = [0; 4096];
            content[..bytes.len()].copy_from_slice(bytes);
            if content.iter().any(|byte| *byte != 0) {
                pages.push((((index as u64 + 1) << 32) | page as u64, Arc::new(content)));
            }
        }
    }
    pages
}
pub(crate) fn export(
    body: &Body,
    memory: &[u8],
    sections: &Sections,
    base: Option<&SparseSnapshot>,
) -> Result<SparseSnapshot> {
    SparseSnapshot::from_parts(
        0,
        body.execution,
        pages(memory, sections),
        &body.encode()?,
        base,
    )
    .map_err(error)
}
pub(crate) fn decode(
    module: &AdmittedModule,
    snapshot: &SparseSnapshot,
    factory: &ServiceFactory,
) -> Result<(Capture, Meter, InputSpec, Sections, Body)> {
    if snapshot.base() != 0 || snapshot.image_identity() != module.execution_digest() {
        return Err(error("portable snapshot execution identity differs"));
    }
    let sidecar = snapshot.sidecar();
    if sidecar.len() > SIDECAR_LIMIT {
        return Err(error("portable snapshot sidecar exceeds its bound"));
    }
    let body = Body::decode(&sidecar)?;
    if body.encode()? != sidecar || body.version != 1 || body.execution != module.execution_digest()
    {
        return Err(error(
            "portable snapshot sidecar is noncanonical or incompatible",
        ));
    }
    if body.blobs.iter().any(|blob| blob.len > LIMIT as u64) {
        return Err(error("portable snapshot section exceeds its bound"));
    }
    let mut memory = vec![0; module.profile().memory_pages as usize * 65536];
    let mut blobs: [Vec<u8>; 4] =
        std::array::from_fn(|index| vec![0; body.blobs[index].len as usize]);
    let maximum_pages = memory.len() / 4096
        + blobs
            .iter()
            .map(|bytes| bytes.len().div_ceil(4096))
            .sum::<usize>();
    if snapshot.pages().len() > maximum_pages {
        return Err(error(
            "portable snapshot exceeds its declared page inventory",
        ));
    }
    let mut previous = None;
    for (gfn, page) in snapshot.pages() {
        if previous.is_some_and(|previous| previous >= *gfn) || page.iter().all(|byte| *byte == 0) {
            return Err(error("portable snapshot pages are not canonical"));
        }
        previous = Some(*gfn);
        let section = gfn >> 32;
        let page_number = gfn & u64::from(u32::MAX);
        let bytes = if section == 0 {
            &mut memory
        } else {
            blobs
                .get_mut((section - 1) as usize)
                .ok_or_else(|| error("portable snapshot page names an unknown section"))?
        };
        let at = usize::try_from(page_number)
            .ok()
            .and_then(|page| page.checked_mul(4096))
            .filter(|at| *at < bytes.len())
            .ok_or_else(|| error("portable snapshot page exceeds its section"))?;
        let take = (bytes.len() - at).min(4096);
        if page[take..].iter().any(|byte| *byte != 0) {
            return Err(error("portable snapshot tail padding is nonzero"));
        }
        bytes[at..at + take].copy_from_slice(&page[..take]);
    }
    if *blake3::hash(&memory).as_bytes() != body.memory {
        return Err(error("portable memory checksum differs"));
    }
    for (blob, expected) in blobs.iter().zip(&body.blobs) {
        if <[u8; 32]>::from(Sha256::digest(blob)) != expected.digest {
            return Err(error("portable host-state checksum differs"));
        }
    }
    if module.identity_with_input(&blobs[0]) != body.input {
        return Err(error("portable input identity differs"));
    }
    let input = InputSpec::decode_snapshot(&blobs[0]).map_err(error)?;
    if !input.effects().is_empty() || !input.reseeds().is_empty() {
        return Err(error(
            "portable artifact declares unsupported machine effects",
        ));
    }
    let mut env = input.materialize(factory).map_err(error)?;
    RecordedState::decode(&blobs[1])
        .map_err(error)?
        .restore_into(&mut env)
        .map_err(error)?;
    let sections = Sections(blobs.map(|bytes| SharedState::from_bytes(bytes, None)));
    let mut host = Host::new(env);
    host.pending = body.pending.clone();
    host.observations = body.observations.clone();
    host.thresholds = body.thresholds.clone();
    host.closed = body.closed;
    host.imports = body.imports;
    host.events = sections.0[2].clone();
    host.console = sections.0[3].clone();
    let mut last = 0;
    let moment = body.meter.moment(body.fuel)?;
    for (at, _, _) in host.events()? {
        if at < last || at > moment {
            return Err(error("portable evidence moments are invalid"));
        }
        last = at;
    }
    if host.observations.len() > hypercall_proto::observation::MAX_REGIONS as usize
        || host.observations.iter().any(|(handle, (address, len))| {
            *handle == 0
                || *len == 0
                || *len > hypercall_proto::observation::MAX_LEN
                || range(&memory, *address, u64::from(*len)).is_err()
        })
    {
        return Err(error("portable observation registrations are invalid"));
    }
    if body.continuation.as_ref().is_some_and(|continuation| {
        continuation.values.len() >= module.profile().stack_registers as usize
            || continuation.frames.len() > module.profile().recursion_depth as usize
            || continuation.required.is_some_and(|required| {
                required > Meter::maximum_deadline_overshoot(module.profile())
            })
    }) {
        return Err(error("portable continuation exceeds its admitted limits"));
    }
    if (host.pending.is_some()
        != body
            .continuation
            .as_ref()
            .is_some_and(|continuation| continuation.result.is_some()))
        || (body.finished || body.trap.is_some())
            && (host.pending.is_some() || body.continuation.is_some())
        || !body.started && (body.continuation.is_some() || body.finished || body.trap.is_some())
        || body.started && !body.finished && body.trap.is_none() && body.continuation.is_none()
    {
        return Err(error("portable execution phase is inconsistent"));
    }
    let capture = Capture {
        memory,
        globals: body.globals.clone(),
        tables: body.tables.clone(),
        fuel: body.fuel,
        host,
        continuation: body.continuation.clone(),
        data: body.data.clone(),
        elements: body.elements.clone(),
        invocation: body.invocation.clone(),
        started: body.started,
        finished: body.finished,
        trap: body.trap.clone(),
        outputs: body.outputs.clone(),
    };
    Ok((capture, body.meter.clone(), input, sections, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{WasmSession, admission::Profile};
    use consonance_client::session::SearchSession;
    use environment::input_spec::nominal_factory;
    #[test]
    fn rechecksummed_invalid_artifacts_cannot_commit_restore() {
        let source = wat::parse_str("(module (memory 1 1) (global $g (mut i64) (i64.const 1234567890123)) (func (export \"run\") loop global.get $g i64.const 1 i64.add global.set $g br 0 end))").unwrap();
        let profile = Profile {
            memory_pages: 1,
            ..Profile::default()
        };
        let module = AdmittedModule::new(&source, profile).unwrap();
        let mut session = WasmSession::new(
            module,
            InputSpec::seeded(4),
            Invocation::new("run"),
            nominal_factory(),
        )
        .unwrap();
        session.run_until(4096).unwrap();
        let id = session.snapshot().unwrap().0;
        let artifact = session.export_sparse_snapshot(id, None).unwrap();
        let original = Body::decode(&artifact.sidecar()).unwrap();
        let mut bad = original.clone();
        bad.continuation.as_mut().unwrap().frames[0][2] = u64::MAX;
        let mut cases = vec![bad];
        let mut bad = original.clone();
        bad.globals.clear();
        cases.push(bad);
        let mut bad = original.clone();
        bad.continuation.as_mut().unwrap().values.pop();
        cases.push(bad);
        let mut bad = original.clone();
        bad.outputs.push(Scalar::I32(0));
        cases.push(bad);
        let mut bad = original.clone();
        bad.pending = Some(Pending::Close(1));
        cases.push(bad);
        let mut bad = original.clone();
        bad.blobs[0].len = u64::MAX;
        cases.push(bad);
        let before = session.state_hash().unwrap();
        let bytes = session.store_bytes().unwrap();
        for body in cases {
            let broken = SparseSnapshot::from_parts(
                0,
                artifact.image_identity(),
                artifact.pages().to_vec(),
                &body.encode().unwrap(),
                None,
            )
            .unwrap();
            assert!(session.import_sparse_snapshot(&broken).is_err());
            assert_eq!(session.state_hash().unwrap(), before);
            assert_eq!(session.store_bytes().unwrap(), bytes);
        }
        let mut page = [0; 4096];
        page[0] = 1;
        let mut pages = artifact.pages().to_vec();
        pages.push((5 << 32, Arc::new(page)));
        let broken = SparseSnapshot::from_parts(
            0,
            artifact.image_identity(),
            pages,
            &artifact.sidecar(),
            None,
        )
        .unwrap();
        assert!(session.import_sparse_snapshot(&broken).is_err());
        assert_eq!(session.state_hash().unwrap(), before);
    }
}
