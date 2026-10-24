// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::channel::{
    Answer, ChannelError, CoverageSchedule, Effect, MAX_CHANNEL_BYTES, MAX_RECORDED_STATE_BYTES,
    NominalHandler, RecordedEnv, ServiceHandler,
};
use std::{collections::BTreeMap, sync::Arc};

use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceConfig {
    pub identity: Vec<u8>,
    pub configuration: Vec<u8>,
    pub local_services: Vec<u16>,
}
impl Default for ServiceConfig {
    fn default() -> Self {
        let handler = NominalHandler;
        Self {
            identity: handler.identity().to_vec(),
            configuration: handler.configuration().to_vec(),
            local_services: handler.local_services().to_vec(),
        }
    }
}
impl ServiceConfig {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode_into(&mut out);
        out
    }
    #[inline]
    fn encode_into(&self, out: &mut Vec<u8>) {
        put(out, &self.identity);
        put(out, &self.configuration);
        let policy: Vec<_> = self
            .local_services
            .iter()
            .flat_map(|id| id.to_le_bytes())
            .collect();
        put(out, &policy);
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, ChannelError> {
        let mut r = Reader(bytes);
        let result = Self {
            identity: r.bytes()?.to_vec(),
            configuration: r.bytes()?.to_vec(),
            local_services: crate::channel::decode_local_services(r.bytes()?)?,
        };
        r.finish()?;
        Ok(result)
    }
}
pub type ServiceFactory =
    Arc<dyn Fn(&ServiceConfig) -> Result<Box<dyn ServiceHandler>, ChannelError> + Send + Sync>;
pub fn nominal_factory() -> ServiceFactory {
    Arc::new(|config| {
        if config != &ServiceConfig::default() {
            return Err(ChannelError::Handler(
                "service implementation is not installed".into(),
            ));
        }
        Ok(Box::new(NominalHandler))
    })
}
#[derive(Clone, Debug)]
pub struct InputSpec {
    seed: u64,
    config: ServiceConfig,
    effects: BTreeMap<u64, Effect>,
    reseeds: BTreeMap<u64, u64>,
    payloads: Option<Vec<Vec<u8>>>,
    coverage_schedule: Option<CoverageSchedule>,
    answers: BTreeMap<(u64, u16, u64), Answer>,
    request_bindings: BTreeMap<(u64, u16, u64), [u8; 32]>,
    encoded_len: usize,
}
impl PartialEq for InputSpec {
    fn eq(&self, other: &Self) -> bool {
        self.seed == other.seed
            && self.config == other.config
            && self.effects == other.effects
            && self.reseeds == other.reseeds
            && self.payloads == other.payloads
            && self.coverage_schedule == other.coverage_schedule
            && self.answers == other.answers
            && self.request_bindings == other.request_bindings
    }
}
impl Eq for InputSpec {}
impl InputSpec {
    pub const BLOB_VERSION: u16 = 8;
    pub const MAX_REPRODUCER_BYTES: usize = MAX_RECORDED_STATE_BYTES - MAX_CHANNEL_BYTES;
    pub fn seeded(seed: u64) -> Self {
        let mut spec = Self {
            seed,
            config: ServiceConfig::default(),
            effects: BTreeMap::new(),
            reseeds: BTreeMap::new(),
            payloads: None,
            coverage_schedule: None,
            answers: BTreeMap::new(),
            request_bindings: BTreeMap::new(),
            encoded_len: 0,
        };
        spec.encoded_len = spec.computed_len();
        spec
    }
    pub fn seed(&self) -> u64 {
        self.seed
    }
    pub fn config(&self) -> &ServiceConfig {
        &self.config
    }
    pub fn set_config(&mut self, config: ServiceConfig) {
        self.encoded_len = self.encoded_len - config_len(&self.config) + config_len(&config);
        self.config = config;
    }
    pub fn effects(&self) -> &BTreeMap<u64, Effect> {
        &self.effects
    }
    pub fn reseeds(&self) -> &BTreeMap<u64, u64> {
        &self.reseeds
    }
    pub fn payloads(&self) -> Option<&[Vec<u8>]> {
        self.payloads.as_deref()
    }
    pub fn set_payloads(&mut self, payloads: Option<Vec<Vec<u8>>>) {
        self.encoded_len = self.encoded_len - payloads_len(self.payloads.as_deref())
            + payloads_len(payloads.as_deref());
        self.payloads = payloads;
    }
    pub fn coverage_schedule(&self) -> Option<&CoverageSchedule> {
        self.coverage_schedule.as_ref()
    }
    pub fn set_coverage_schedule(&mut self, schedule: Option<CoverageSchedule>) {
        self.encoded_len = self.encoded_len - schedule_len(self.coverage_schedule.as_ref())
            + schedule_len(schedule.as_ref());
        self.coverage_schedule = schedule;
    }
    pub fn record_reseed(&mut self, at: u64, seed: u64) {
        if self.reseeds.insert(at, seed).is_none() {
            self.encoded_len += RESEED_ENTRY_LEN;
        }
    }
    pub fn record_effect(&mut self, at: u64, effect: Effect) {
        self.encoded_len += effect_entry_len(&effect);
        if let Some(previous) = self.effects.insert(at, effect) {
            self.encoded_len -= effect_entry_len(&previous);
        }
    }
    pub fn answers(&self) -> &BTreeMap<(u64, u16, u64), Answer> {
        &self.answers
    }
    pub fn request_bindings(&self) -> &BTreeMap<(u64, u16, u64), [u8; 32]> {
        &self.request_bindings
    }
    pub fn record_answer(
        &mut self,
        at: u64,
        service: u16,
        request: u64,
        answer: Answer,
    ) -> Result<(), ChannelError> {
        if answer
            .payload()
            .is_some_and(|bytes| bytes.len() > MAX_CHANNEL_BYTES)
        {
            return Err(ChannelError::TooLarge);
        }
        let key = (at, service, request);
        self.encoded_len += answer_entry_len(&answer, false);
        let previous = self.answers.insert(key, answer);
        let was_bound = self.request_bindings.remove(&key).is_some();
        if let Some(previous) = previous {
            self.encoded_len -= answer_entry_len(&previous, was_bound);
        }
        Ok(())
    }
    pub fn record_bound_answer(
        &mut self,
        at: u64,
        service: u16,
        request: u64,
        request_sha256: [u8; 32],
        answer: Answer,
    ) -> Result<(), ChannelError> {
        self.record_answer(at, service, request, answer)?;
        self.request_bindings
            .insert((at, service, request), request_sha256);
        self.encoded_len += Sha256::output_size();
        Ok(())
    }

    pub fn encoded_len(&self) -> usize {
        self.encoded_len
    }

    pub fn projected_reproducer_len_with_binding_reserve(
        &self,
        effects: &BTreeMap<u64, Effect>,
        reseeds: &BTreeMap<u64, u64>,
        key: (u64, u16, u64),
        answer: &Answer,
    ) -> Result<usize, ChannelError> {
        if answer
            .payload()
            .is_some_and(|bytes| bytes.len() > MAX_CHANNEL_BYTES)
        {
            return Err(ChannelError::TooLarge);
        }
        let mut len = self.encoded_len;
        let mut unbound = self.answers.len() - self.request_bindings.len();
        for (at, effect) in effects {
            len += effect_entry_len(effect);
            if let Some(previous) = self.effects.get(at) {
                len -= effect_entry_len(previous);
            }
        }
        len += RESEED_ENTRY_LEN
            * reseeds
                .keys()
                .filter(|at| !self.reseeds.contains_key(at))
                .count();
        len += answer_entry_len(answer, true);
        if let Some(previous) = self.answers.get(&key) {
            let was_bound = self.request_bindings.contains_key(&key);
            len -= answer_entry_len(previous, was_bound);
            unbound -= usize::from(!was_bound);
        }
        Ok(len + unbound * Sha256::output_size())
    }

    fn computed_len(&self) -> usize {
        HEADER_LEN
            + config_len(&self.config)
            + COUNT_LEN
            + self.effects.values().map(effect_entry_len).sum::<usize>()
            + COUNT_LEN
            + RESEED_ENTRY_LEN * self.reseeds.len()
            + payloads_len(self.payloads.as_deref())
            + schedule_len(self.coverage_schedule.as_ref())
            + COUNT_LEN
            + self
                .answers
                .iter()
                .map(|(key, answer)| {
                    answer_entry_len(answer, self.request_bindings.contains_key(key))
                })
                .sum::<usize>()
    }
    pub fn materialize(
        &self,
        factory: &ServiceFactory,
    ) -> Result<RecordedEnv<Box<dyn ServiceHandler>>, ChannelError> {
        crate::channel::validate_local_services(&self.config.local_services)?;
        let handler = factory(&self.config)?;
        if handler.identity() != self.config.identity
            || handler.configuration() != self.config.configuration
            || handler.local_services() != self.config.local_services
        {
            return Err(ChannelError::Handler(
                "factory returned a different service contract".into(),
            ));
        }
        let mut env = RecordedEnv::new(self.seed, handler);
        if env.handler().identity() != self.config.identity
            || env.handler().configuration() != self.config.configuration
            || env.handler().local_services() != self.config.local_services
        {
            return Err(ChannelError::Handler(
                "reseed changed the service contract".into(),
            ));
        }
        env.set_payloads(self.payloads.clone())?;
        env.set_coverage_schedule(self.coverage_schedule.clone());
        for (&(at, service, request), answer) in &self.answers {
            match self.request_bindings.get(&(at, service, request)) {
                Some(&digest) => {
                    env.record_bound_service_request(at, service, request, digest, answer.clone())
                }
                None => env.record_service_request(at, service, request, answer.clone()),
            }
        }
        Ok(env)
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut out = b"HENV".to_vec();
        out.extend(Self::BLOB_VERSION.to_le_bytes());
        out.extend(self.seed.to_le_bytes());
        put_encoded(&mut out, |out| self.config.encode_into(out));
        out.extend((self.effects.len() as u32).to_le_bytes());
        for (at, effect) in &self.effects {
            out.extend(at.to_le_bytes());
            put_encoded(&mut out, |out| effect.encode_into(out));
        }
        out.extend((self.reseeds.len() as u32).to_le_bytes());
        for (at, seed) in &self.reseeds {
            out.extend(at.to_le_bytes());
            out.extend(seed.to_le_bytes());
        }
        match &self.payloads {
            None => out.push(0),
            Some(payloads) => {
                out.push(1);
                out.extend((payloads.len() as u32).to_le_bytes());
                for p in payloads {
                    put(&mut out, p);
                }
            }
        }
        match &self.coverage_schedule {
            None => out.push(0),
            Some(schedule) => {
                out.push(1);
                put(&mut out, &schedule.encode());
            }
        }
        out.extend((self.answers.len() as u32).to_le_bytes());
        for ((at, service, request), answer) in &self.answers {
            out.extend(at.to_le_bytes());
            out.extend(service.to_le_bytes());
            out.extend(request.to_le_bytes());
            let digest = self.request_bindings.get(&(*at, *service, *request));
            match (answer, digest) {
                (Answer::Nominal, None) => out.push(0),
                (Answer::Data(bytes), None) => {
                    out.push(1);
                    put(&mut out, bytes);
                }
                (Answer::Nominal, Some(digest)) => {
                    out.push(2);
                    out.extend(digest);
                }
                (Answer::Data(bytes), Some(digest)) => {
                    out.push(3);
                    out.extend(digest);
                    put(&mut out, bytes);
                }
            }
        }
        out
    }
    pub fn try_encode(&self) -> Result<Vec<u8>, ChannelError> {
        let bytes = self.encode();
        Self::decode(&bytes)?;
        Ok(bytes)
    }
    pub fn try_encode_reproducer(&self) -> Result<Vec<u8>, ChannelError> {
        let bytes = self.encode();
        Self::decode_reproducer(&bytes)?;
        Ok(bytes)
    }

    pub fn try_encode_reproducer_with_binding_reserve(&self) -> Result<Vec<u8>, ChannelError> {
        let bytes = self.try_encode_reproducer()?;
        let unbound = self.answers.len() - self.request_bindings.len();
        if bytes
            .len()
            .saturating_add(unbound.saturating_mul(Sha256::output_size()))
            > Self::MAX_REPRODUCER_BYTES
        {
            return Err(ChannelError::TooLarge);
        }
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, ChannelError> {
        if bytes.len() > MAX_CHANNEL_BYTES {
            return Err(ChannelError::TooLarge);
        }
        Self::decode_snapshot(bytes)
    }

    pub fn decode_reproducer(bytes: &[u8]) -> Result<Self, ChannelError> {
        if bytes.len() > Self::MAX_REPRODUCER_BYTES {
            return Err(ChannelError::TooLarge);
        }
        Self::decode_snapshot(bytes)
    }

    pub fn decode_snapshot(bytes: &[u8]) -> Result<Self, ChannelError> {
        let mut r = Reader(bytes);
        if r.take(4)? != b"HENV" || r.u16()? != Self::BLOB_VERSION {
            return Err(ChannelError::Malformed);
        }
        let mut result = Self::seeded(r.u64()?);
        result.config = ServiceConfig::decode(r.bytes()?)?;
        let mut previous = None;
        for _ in 0..r.count(13)? {
            let at = r.u64()?;
            if previous.is_some_and(|p| p >= at) {
                return Err(ChannelError::Malformed);
            }
            previous = Some(at);
            result.effects.insert(at, Effect::decode(r.bytes()?)?);
        }
        previous = None;
        for _ in 0..r.count(16)? {
            let at = r.u64()?;
            if previous.is_some_and(|p| p >= at) {
                return Err(ChannelError::Malformed);
            }
            previous = Some(at);
            result.reseeds.insert(at, r.u64()?);
        }
        result.payloads = match r.take(1)?[0] {
            0 => None,
            1 => {
                let count = r.count(4)?;
                let mut payloads = Vec::new();
                for _ in 0..count {
                    payloads.push(r.bytes()?.to_vec());
                }
                Some(payloads)
            }
            _ => return Err(ChannelError::Malformed),
        };
        result.coverage_schedule = match r.take(1)?[0] {
            0 => None,
            1 => Some(CoverageSchedule::decode(r.bytes()?)?),
            _ => return Err(ChannelError::Malformed),
        };
        let mut previous = None;
        for _ in 0..r.count(19)? {
            let key = (r.u64()?, r.u16()?, r.u64()?);
            if previous.is_some_and(|last| last >= key) {
                return Err(ChannelError::Malformed);
            }
            previous = Some(key);
            let tag = r.take(1)?[0];
            if matches!(tag, 2 | 3) {
                let mut digest = [0; 32];
                digest.copy_from_slice(r.take(32)?);
                result.request_bindings.insert(key, digest);
            }
            let answer = match tag {
                0 | 2 => Answer::Nominal,
                1 | 3 => Answer::data(r.bytes()?.to_vec())?,
                _ => return Err(ChannelError::Malformed),
            };
            result.answers.insert(key, answer);
        }
        r.finish()?;
        result.encoded_len = result.computed_len();
        Ok(result)
    }
}

const HEADER_LEN: usize = 14;
const COUNT_LEN: usize = 4;
const RESEED_ENTRY_LEN: usize = 16;

fn config_len(config: &ServiceConfig) -> usize {
    COUNT_LEN + config.encode().len()
}

fn effect_entry_len(effect: &Effect) -> usize {
    8 + COUNT_LEN + effect.encode().len()
}

fn payloads_len(payloads: Option<&[Vec<u8>]>) -> usize {
    1 + payloads.map_or(0, |payloads| {
        COUNT_LEN
            + payloads
                .iter()
                .map(|payload| COUNT_LEN + payload.len())
                .sum::<usize>()
    })
}

fn schedule_len(schedule: Option<&CoverageSchedule>) -> usize {
    1 + schedule.map_or(0, |schedule| COUNT_LEN + schedule.encode().len())
}

fn answer_entry_len(answer: &Answer, bound: bool) -> usize {
    let binding = if bound { Sha256::output_size() } else { 0 };
    let data = answer.payload().map_or(0, |bytes| COUNT_LEN + bytes.len());
    19 + binding + data
}
fn put_encoded(out: &mut Vec<u8>, encode: impl FnOnce(&mut Vec<u8>)) {
    let length_at = out.len();
    out.extend(0_u32.to_le_bytes());
    let start = out.len();
    encode(out);
    let length = out.len() - start;
    out[length_at..start].copy_from_slice(&(length as u32).to_le_bytes());
}

fn put(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend((bytes.len() as u32).to_le_bytes());
    out.extend(bytes);
}
struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ChannelError> {
        let (head, tail) = self.0.split_at_checked(n).ok_or(ChannelError::Malformed)?;
        self.0 = tail;
        Ok(head)
    }
    fn u16(&mut self) -> Result<u16, ChannelError> {
        Ok(u16::from_le_bytes(
            self.take(2)?
                .try_into()
                .map_err(|_| ChannelError::Malformed)?,
        ))
    }
    fn u32(&mut self) -> Result<u32, ChannelError> {
        Ok(u32::from_le_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| ChannelError::Malformed)?,
        ))
    }
    fn u64(&mut self) -> Result<u64, ChannelError> {
        Ok(u64::from_le_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| ChannelError::Malformed)?,
        ))
    }
    fn bytes(&mut self) -> Result<&'a [u8], ChannelError> {
        let len = self.u32()? as usize;
        if len > MAX_CHANNEL_BYTES {
            return Err(ChannelError::TooLarge);
        }
        self.take(len)
    }
    fn count(&mut self, minimum: usize) -> Result<u32, ChannelError> {
        let n = self.u32()?;
        if n as usize > self.0.len() / minimum {
            return Err(ChannelError::Malformed);
        }
        Ok(n)
    }
    fn finish(self) -> Result<(), ChannelError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(ChannelError::Malformed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::RecordedState;

    #[derive(Clone)]
    struct ReseedChangesContract {
        reseeded: bool,
        change_identity: bool,
    }

    impl ServiceHandler for ReseedChangesContract {
        fn identity(&self) -> &[u8] {
            if self.reseeded && self.change_identity {
                b"other-contract"
            } else {
                b"reseed-changes-contract"
            }
        }

        fn configuration(&self) -> &[u8] {
            if self.reseeded && !self.change_identity {
                b"after"
            } else {
                b"before"
            }
        }

        fn respond(
            &mut self,
            _moment: crate::Moment,
            _question: &crate::channel::Question,
        ) -> Result<crate::channel::ServiceResponse, ChannelError> {
            Ok(crate::channel::ServiceResponse::Answered(Answer::Data(
                self.configuration().to_vec(),
            )))
        }

        fn snapshot_state(&self) -> Result<Vec<u8>, ChannelError> {
            Ok(Vec::new())
        }

        fn restore_state(&mut self, state: &[u8]) -> Result<(), ChannelError> {
            if state.is_empty() {
                Ok(())
            } else {
                Err(ChannelError::Malformed)
            }
        }

        fn reseed(&mut self, _seed: u64) {
            self.reseeded = true;
        }

        fn clone_box(&self) -> Box<dyn ServiceHandler> {
            Box::new(self.clone())
        }
    }

    #[test]
    fn local_policy_roundtrip_rejects_noncanonical_bytes_and_factory_mismatch() {
        let mut spec = InputSpec::seeded(7);
        spec.config.local_services = vec![19, 25];
        assert_eq!(InputSpec::decode(&spec.encode()).unwrap(), spec);
        assert!(spec.materialize(&nominal_factory()).is_err());
        let wrong: ServiceFactory = Arc::new(|_| Ok(Box::new(NominalHandler)));
        assert!(spec.materialize(&wrong).is_err());
        for policy in [vec![19, 19], vec![25, 19], vec![4]] {
            spec.config.local_services = policy;
            assert!(InputSpec::decode(&spec.encode()).is_err());
            assert!(spec.materialize(&wrong).is_err());
        }
        let mut old = InputSpec::seeded(7).encode();
        old[4..6].copy_from_slice(&7_u16.to_le_bytes());
        assert!(InputSpec::decode(&old).is_err());
    }

    #[test]
    fn materialize_rejects_service_contract_changed_by_initial_reseed() {
        let mut spec = InputSpec::seeded(7);
        spec.set_config(ServiceConfig {
            local_services: Vec::new(),
            identity: b"reseed-changes-contract".to_vec(),
            configuration: b"before".to_vec(),
        });
        for change_identity in [false, true] {
            let factory: ServiceFactory = Arc::new(move |_| {
                Ok(Box::new(ReseedChangesContract {
                    reseeded: false,
                    change_identity,
                }))
            });
            assert!(spec.materialize(&factory).is_err());
        }
    }

    #[test]
    fn coverage_schedule_survives_reproducer_and_materialization() {
        let schedule = CoverageSchedule::new(1024, vec![(10, 20, 1), (30, 40, 9)]).unwrap();
        let mut spec = InputSpec::seeded(41);
        spec.set_coverage_schedule(Some(schedule.clone()));
        let replay = InputSpec::decode_reproducer(&spec.try_encode_reproducer().unwrap()).unwrap();
        assert_eq!(replay.coverage_schedule(), Some(&schedule));
        let env = replay.materialize(&nominal_factory()).unwrap();
        assert_eq!(env.coverage_schedule(), Some(&schedule));
        let state = RecordedState::decode(&env.snapshot_state().unwrap().encode()).unwrap();
        let mut restored = InputSpec::seeded(41)
            .materialize(&nominal_factory())
            .unwrap();
        state.restore_into(&mut restored).unwrap();
        assert_eq!(restored.coverage_schedule(), Some(&schedule));
    }
    mod buffered_reference {
        use crate as environment;
        include!("../qualification/reference-input-spec.rs");

        pub(super) fn encode(spec: &crate::input_spec::InputSpec) -> Vec<u8> {
            spec.encode_buffered()
        }
    }

    #[test]
    fn nested_encodings_match_buffered_wire_for_every_effect_and_tape_shape() {
        for bytes in [0, 1, 256, 4096] {
            for payloads in [None, Some(vec![]), Some(vec![vec![], vec![1, 2, 3]])] {
                let mut spec = InputSpec::seeded(0x1234_5678);
                spec.set_config(ServiceConfig {
                    local_services: Vec::new(),
                    identity: vec![0x47; bytes],
                    configuration: vec![1, 2, 3],
                });
                spec.record_effect(
                    3,
                    Effect::WriteMemory {
                        gpa: 0x1234,
                        bytes: vec![5; bytes],
                    },
                );
                spec.record_effect(
                    5,
                    Effect::XorMemory {
                        gpa: 0x5678,
                        bytes: vec![9; bytes],
                    },
                );
                spec.record_reseed(9, 0x9876);
                spec.set_payloads(payloads);
                spec.record_answer(11, 19, 3, Answer::Data(vec![4, 5]))
                    .unwrap();
                spec.record_bound_answer(11, 19, 4, [0x73; 32], Answer::Data(vec![6, 7]))
                    .unwrap();
                spec.record_answer(11, 19, 7, Answer::Nominal).unwrap();
                spec.record_bound_answer(11, 19, 8, [0xa5; 32], Answer::Nominal)
                    .unwrap();
                let encoded = spec.encode();
                assert_eq!(encoded, buffered_reference::encode(&spec));
                assert_eq!(InputSpec::decode_snapshot(&encoded).unwrap(), spec);
            }
        }
    }

    #[test]
    fn inputs_round_trip_and_reject_every_truncation() {
        let mut spec = InputSpec::seeded(41);
        spec.record_effect(12, Effect::write_memory(4096, vec![7, 8]).unwrap());
        spec.record_effect(20, Effect::xor_memory(0x20, vec![32]).unwrap());
        spec.record_reseed(8, 92);
        spec.set_payloads(Some(vec![vec![], vec![1, 2, 3]]));
        let bytes = spec.encode();
        assert_eq!(InputSpec::decode(&bytes).unwrap(), spec);
        for end in 0..bytes.len() {
            assert!(InputSpec::decode(&bytes[..end]).is_err(), "prefix {end}");
        }
        let mut trailing = bytes;
        trailing.push(0);
        assert!(InputSpec::decode(&trailing).is_err());
    }
    #[test]
    fn absent_and_exhausted_input_tapes_are_distinct() {
        let absent = InputSpec::seeded(0);
        let mut exhausted = absent.clone();
        exhausted.set_payloads(Some(vec![]));
        assert_ne!(absent.encode(), exhausted.encode());
        assert_eq!(
            InputSpec::decode(&exhausted.encode()).unwrap().payloads(),
            Some(&[][..])
        );
    }
    #[test]
    fn an_uninstalled_extension_cannot_be_materialized() {
        let factory = nominal_factory();
        let mut spec = InputSpec::seeded(1);
        assert!(spec.materialize(&factory).is_ok());
        spec.set_config(ServiceConfig {
            local_services: Vec::new(),
            identity: b"unknown-extension".to_vec(),
            configuration: vec![],
        });
        assert!(spec.materialize(&factory).is_err());
    }
    #[test]
    fn recorded_answers_preserve_service_and_request_identity() {
        use crate::channel::{Question, ServiceResponse};
        let mut spec = InputSpec::seeded(0);
        spec.record_answer(9, 10, 41, Answer::data(vec![4]).unwrap())
            .unwrap();
        spec.record_answer(9, 11, 41, Answer::data(vec![8]).unwrap())
            .unwrap();
        let spec = InputSpec::decode(&spec.encode()).unwrap();
        let mut env = spec.materialize(&nominal_factory()).unwrap();
        env.set_moment(9);
        for (service, expected) in [(10, vec![4]), (11, vec![8])] {
            let question = Question::with_request_id(41, service, vec![]).unwrap();
            assert_eq!(
                env.decide(&question).unwrap(),
                ServiceResponse::Answered(Answer::Data(expected))
            );
        }
        let unknown = Question::with_request_id(42, 10, vec![]).unwrap();
        assert_eq!(
            env.decide(&unknown).unwrap(),
            ServiceResponse::Answered(Answer::Nominal)
        );
    }

    #[test]
    fn bound_answer_reproducer_rejects_a_changed_request_after_materialization() {
        use crate::channel::Question;
        use sha2::{Digest, Sha256};

        let mut spec = InputSpec::seeded(7);
        let digest = Sha256::digest(b"first").into();
        spec.record_bound_answer(4, 19, 11, digest, Answer::Data(vec![42]))
            .unwrap();
        let replay = InputSpec::decode_reproducer(&spec.try_encode_reproducer().unwrap()).unwrap();
        assert_eq!(replay.request_bindings().get(&(4, 19, 11)), Some(&digest));
        let mut env = replay.materialize(&nominal_factory()).unwrap();
        env.set_moment(4);
        let changed = Question::with_request_id(11, 19, b"second".to_vec()).unwrap();
        assert_eq!(
            env.decide(&changed),
            Err(ChannelError::RequestIdentityMismatch)
        );
    }
    #[test]
    fn impossible_counts_do_not_allocate() {
        let spec = InputSpec::seeded(0);
        let mut bytes = spec.encode();
        let offset = 4 + 2 + 8 + 4 + spec.config.encode().len();
        bytes[offset..offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(InputSpec::decode(&bytes).is_err());
    }
    #[test]
    fn builder_accessors_and_replacement_preserve_distinct_fields() {
        let mut spec = InputSpec::seeded(0x1234);
        assert_eq!(spec.seed(), 0x1234);
        assert_eq!(spec.config(), &ServiceConfig::default());
        assert_eq!(spec.payloads(), None);
        assert!(spec.effects().is_empty());
        assert!(spec.reseeds().is_empty());
        assert!(spec.answers().is_empty());
        let config = ServiceConfig {
            local_services: Vec::new(),
            identity: b"service".to_vec(),
            configuration: b"options".to_vec(),
        };
        spec.set_config(config.clone());
        assert_eq!(spec.config(), &config);
        spec.record_effect(12, Effect::xor_memory(0x20, vec![32]).unwrap());
        spec.record_effect(12, Effect::xor_memory(0x20, vec![33]).unwrap());
        spec.record_reseed(13, 41);
        spec.record_reseed(13, 42);
        assert_eq!(
            spec.effects(),
            &BTreeMap::from([(12, Effect::xor_memory(0x20, vec![33]).unwrap())])
        );
        assert_eq!(spec.reseeds(), &BTreeMap::from([(13, 42)]));
        spec.record_answer(14, 10, 9, Answer::Nominal).unwrap();
        spec.record_answer(14, 10, 9, Answer::Data(vec![7]))
            .unwrap();
        assert_eq!(
            spec.answers(),
            &BTreeMap::from([((14, 10, 9), Answer::Data(vec![7]))])
        );
        spec.set_payloads(Some(vec![vec![1, 2]]));
        assert_eq!(spec.payloads(), Some(&[vec![1, 2]][..]));
        assert_eq!(
            InputSpec::decode(&spec.try_encode().unwrap()).unwrap(),
            spec
        );
    }

    #[test]
    fn factories_must_match_identity_and_configuration_independently() {
        let factory = nominal_factory();
        let ignored: ServiceFactory = Arc::new(|_| Ok(Box::new(NominalHandler)));
        for identity in [true, false] {
            let mut spec = InputSpec::seeded(41);
            if identity {
                spec.config.identity.push(9);
            } else {
                spec.config.configuration.push(9);
            }
            assert!(spec.materialize(&factory).is_err());
            assert!(spec.materialize(&ignored).is_err());
        }
        let mut spec = InputSpec::seeded(41);
        let mut expected = RecordedEnv::nominal(41);
        spec.set_payloads(Some(vec![vec![5, 6]]));
        expected.set_payloads(Some(vec![vec![5, 6]])).unwrap();
        let mut actual = spec.materialize(&factory).unwrap();
        assert_eq!(
            actual.snapshot_state().unwrap(),
            expected.snapshot_state().unwrap()
        );
        assert_eq!(actual.pull_payload(2).unwrap(), Some(vec![5, 6]));
        assert_eq!(actual.pull_payload(2).unwrap(), None);
        let failed: ServiceFactory = Arc::new(|_| Err(ChannelError::Malformed));
        assert!(matches!(
            spec.materialize(&failed),
            Err(ChannelError::Malformed)
        ));
    }

    #[test]
    fn answer_bounds_reject_before_replacing_existing_response() {
        let mut spec = InputSpec::seeded(1);
        let key = (9, 10, 11);
        let answer = Answer::Data(vec![7; MAX_CHANNEL_BYTES]);
        spec.record_answer(key.0, key.1, key.2, answer.clone())
            .unwrap();
        assert_eq!(spec.answers().get(&key), Some(&answer));
        assert_eq!(
            spec.record_answer(
                key.0,
                key.1,
                key.2,
                Answer::Data(vec![0; MAX_CHANNEL_BYTES + 1])
            ),
            Err(ChannelError::TooLarge)
        );
        assert_eq!(spec.answers().get(&key), Some(&answer));
        assert_eq!(spec.try_encode(), Err(ChannelError::TooLarge));
    }

    #[test]
    fn complete_reproducer_enforces_exact_envelope_boundary_and_header() {
        let mut spec = InputSpec::seeded(7);
        spec.set_payloads(Some(vec![vec![]]));
        let overhead = spec.encode().len();
        spec.set_payloads(Some(vec![vec![5; MAX_CHANNEL_BYTES - overhead]]));
        let bytes = spec.try_encode().unwrap();
        assert_eq!(bytes.len(), MAX_CHANNEL_BYTES);
        assert_eq!(InputSpec::decode(&bytes).unwrap(), spec);
        let mut oversized = bytes.clone();
        oversized.push(0);
        assert_eq!(InputSpec::decode(&oversized), Err(ChannelError::TooLarge));
        for index in [0, 4, 5] {
            let mut invalid = bytes.clone();
            invalid[index] ^= 1;
            assert_eq!(InputSpec::decode(&invalid), Err(ChannelError::Malformed));
        }
    }

    #[test]
    fn snapshot_decode_allows_aggregate_history_beyond_transport_limit() {
        const EFFECT_BYTES: usize = 400_000;
        let mut spec = InputSpec::seeded(7);
        for (at, fill) in [(1, 0x11), (2, 0x22), (3, 0x33)] {
            spec.record_effect(
                at,
                Effect::write_memory(at, vec![fill; EFFECT_BYTES]).unwrap(),
            );
        }
        let bytes = spec.encode();
        assert!(bytes.len() > MAX_CHANNEL_BYTES);
        assert_eq!(InputSpec::decode(&bytes), Err(ChannelError::TooLarge));
        assert_eq!(InputSpec::try_encode_reproducer(&spec).unwrap(), bytes);
        assert_eq!(InputSpec::decode_reproducer(&bytes).unwrap(), spec);
        assert_eq!(InputSpec::decode_snapshot(&bytes).unwrap(), spec);
        assert_eq!(
            InputSpec::decode_reproducer(&vec![0; InputSpec::MAX_REPRODUCER_BYTES + 1]),
            Err(ChannelError::TooLarge)
        );
    }

    #[test]
    fn snapshot_decode_rejects_an_oversized_individual_value() {
        let mut spec = InputSpec::seeded(0);
        spec.record_effect(1, Effect::write_memory(0, vec![7]).unwrap());
        let mut bytes = spec.encode();
        let effect_count_offset = 4 + 2 + 8 + 4 + spec.config.encode().len();
        let value_len_offset = effect_count_offset + 4 + 8 + 4 + 1 + 8;
        bytes[value_len_offset..value_len_offset + 4]
            .copy_from_slice(&((MAX_CHANNEL_BYTES + 1) as u32).to_le_bytes());
        assert_eq!(
            InputSpec::decode_snapshot(&bytes),
            Err(ChannelError::TooLarge)
        );
    }

    #[test]
    fn service_config_is_strict_and_reader_checks_counts_before_consumption() {
        let config = ServiceConfig {
            local_services: Vec::new(),
            identity: b"identifier".to_vec(),
            configuration: vec![1, 2, 3],
        };
        let bytes = config.encode();
        assert_eq!(ServiceConfig::decode(&bytes).unwrap(), config);
        for end in 0..bytes.len() {
            assert!(ServiceConfig::decode(&bytes[..end]).is_err());
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(
            ServiceConfig::decode(&trailing),
            Err(ChannelError::Malformed)
        );
        for (count, available, minimum, accepted) in [
            (0u32, 0, 8, true),
            (1, 8, 8, true),
            (2, 8, 8, false),
            (2, 7, 3, true),
            (3, 7, 3, false),
        ] {
            let mut bytes = count.to_le_bytes().to_vec();
            bytes.resize(4 + available, 0);
            let result = Reader(&bytes).count(minimum);
            if accepted {
                assert_eq!(result.unwrap(), count);
            } else {
                assert_eq!(result, Err(ChannelError::Malformed));
            }
        }
        let mut bytes = (MAX_CHANNEL_BYTES as u32).to_le_bytes().to_vec();
        bytes.resize(4 + MAX_CHANNEL_BYTES, 7);
        assert_eq!(Reader(&bytes).bytes().unwrap().len(), MAX_CHANNEL_BYTES);
        bytes[..4].copy_from_slice(&((MAX_CHANNEL_BYTES + 1) as u32).to_le_bytes());
        assert_eq!(Reader(&bytes).bytes(), Err(ChannelError::TooLarge));
    }

    fn ordered_wire(effects: &[u64], reseeds: &[u64], answers: &[(u64, u16, u64)]) -> Vec<u8> {
        let empty = InputSpec::seeded(13);
        let mut bytes = empty.encode();
        bytes.truncate(18 + empty.config.encode().len());
        bytes.extend((effects.len() as u32).to_le_bytes());
        for at in effects {
            bytes.extend(at.to_le_bytes());
            put(
                &mut bytes,
                &Effect::xor_memory(0x20, vec![32]).unwrap().encode(),
            );
        }
        bytes.extend((reseeds.len() as u32).to_le_bytes());
        for at in reseeds {
            bytes.extend(at.to_le_bytes());
            bytes.extend(43u64.to_le_bytes());
        }
        bytes.push(0);
        bytes.push(0);
        bytes.extend((answers.len() as u32).to_le_bytes());
        for (at, service, request) in answers {
            bytes.extend(at.to_le_bytes());
            bytes.extend(service.to_le_bytes());
            bytes.extend(request.to_le_bytes());
            bytes.push(0);
        }
        bytes
    }

    #[test]
    fn serialized_maps_reject_duplicates_and_descending_keys() {
        for (points, valid) in [(vec![1, 2], true), (vec![2, 2], false), (vec![2, 1], false)] {
            assert_eq!(
                InputSpec::decode(&ordered_wire(&points, &[], &[])).is_ok(),
                valid
            );
            assert_eq!(
                InputSpec::decode(&ordered_wire(&[], &points, &[])).is_ok(),
                valid
            );
        }
        let key = (9, 10, 11);
        for (other, valid) in [
            ((9, 10, 12), true),
            ((9, 11, 0), true),
            ((10, 0, 0), true),
            (key, false),
            ((9, 10, 10), false),
            ((9, 9, 99), false),
            ((8, 99, 99), false),
        ] {
            assert_eq!(
                InputSpec::decode(&ordered_wire(&[], &[], &[key, other])).is_ok(),
                valid
            );
        }
        let mut bad_payload_tag = ordered_wire(&[], &[], &[]);
        let at = bad_payload_tag.len() - 6;
        bad_payload_tag[at] = 2;
        assert_eq!(
            InputSpec::decode(&bad_payload_tag),
            Err(ChannelError::Malformed)
        );
        let mut bad_schedule_tag = ordered_wire(&[], &[], &[]);
        let at = bad_schedule_tag.len() - 5;
        bad_schedule_tag[at] = 2;
        assert_eq!(
            InputSpec::decode(&bad_schedule_tag),
            Err(ChannelError::Malformed)
        );
        let mut bad_answer_tag = ordered_wire(&[], &[], &[key]);
        *bad_answer_tag.last_mut().unwrap() = 2;
        assert_eq!(
            InputSpec::decode(&bad_answer_tag),
            Err(ChannelError::Malformed)
        );
    }

    #[test]
    fn cached_length_and_projection_match_the_encoding() {
        let mut state = 0x1f2e_u64;
        let mut draw = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut spec = InputSpec::seeded(3);
        let mut schedule = BTreeMap::new();
        let mut reseeds = BTreeMap::new();
        for step in 0..2_000_u64 {
            let key = (draw() % 300, (draw() % 3) as u16, draw() % 4);
            let answer = if draw() % 3 == 0 {
                Answer::Nominal
            } else {
                Answer::Data(vec![7; (draw() % 40) as usize])
            };
            match draw() % 9 {
                0 => spec.record_effect(
                    draw() % 50,
                    Effect::write_memory(draw(), vec![1; (draw() % 9) as usize]).unwrap(),
                ),
                1 => spec.record_reseed(draw() % 50, draw()),
                2 => spec.set_payloads(
                    (draw() % 2 == 0)
                        .then(|| vec![vec![2; (draw() % 5) as usize]; (step % 4) as usize]),
                ),
                3 => spec.set_coverage_schedule(
                    (draw() % 2 == 0)
                        .then(|| CoverageSchedule::new(8, vec![(1, 5 + step, 3)]).unwrap()),
                ),
                4 => spec
                    .record_answer(key.0, key.1, key.2, answer.clone())
                    .unwrap(),
                _ => spec
                    .record_bound_answer(key.0, key.1, key.2, [draw() as u8; 32], answer.clone())
                    .unwrap(),
            }
            if draw() % 5 == 0 {
                schedule.insert(
                    draw() % 60,
                    Effect::xor_memory(draw(), vec![3; (draw() % 6) as usize]).unwrap(),
                );
            }
            if draw() % 7 == 0 {
                reseeds.insert(draw() % 60, draw());
            }
            assert_eq!(spec.encoded_len(), spec.encode().len());
            let mut projected = spec.clone();
            for (&at, effect) in &schedule {
                projected.record_effect(at, effect.clone());
            }
            for (&at, &seed) in &reseeds {
                projected.record_reseed(at, seed);
            }
            projected
                .record_bound_answer(key.0, key.1, key.2, [9; 32], answer.clone())
                .unwrap();
            let reserve = (projected.answers.len() - projected.request_bindings.len())
                * Sha256::output_size();
            assert_eq!(
                spec.projected_reproducer_len_with_binding_reserve(
                    &schedule, &reseeds, key, &answer
                ),
                Ok(projected.encode().len() + reserve)
            );
        }
        let decoded = InputSpec::decode_snapshot(&spec.encode()).unwrap();
        assert_eq!(decoded, spec);
        assert_eq!(
            spec.projected_reproducer_len_with_binding_reserve(
                &BTreeMap::new(),
                &BTreeMap::new(),
                (0, 0, 0),
                &Answer::Data(vec![0; MAX_CHANNEL_BYTES + 1])
            ),
            Err(ChannelError::TooLarge)
        );
    }
}
