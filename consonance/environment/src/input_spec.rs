// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::channel::{
    Answer, ChannelError, Effect, MAX_CHANNEL_BYTES, NominalHandler, RecordedEnv, ServiceHandler,
};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceConfig {
    pub identity: Vec<u8>,
    pub configuration: Vec<u8>,
}
impl Default for ServiceConfig {
    fn default() -> Self {
        let handler = NominalHandler;
        Self {
            identity: handler.identity().to_vec(),
            configuration: handler.configuration().to_vec(),
        }
    }
}
impl ServiceConfig {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put(&mut out, &self.identity);
        put(&mut out, &self.configuration);
        out
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, ChannelError> {
        let mut r = Reader(bytes);
        let result = Self {
            identity: r.bytes()?.to_vec(),
            configuration: r.bytes()?.to_vec(),
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputSpec {
    seed: u64,
    config: ServiceConfig,
    effects: BTreeMap<u64, Effect>,
    reseeds: BTreeMap<u64, u64>,
    payloads: Option<Vec<Vec<u8>>>,
    answers: BTreeMap<(u64, u16, u64), Answer>,
}
impl InputSpec {
    pub const BLOB_VERSION: u16 = 5;
    pub fn seeded(seed: u64) -> Self {
        Self {
            seed,
            config: ServiceConfig::default(),
            effects: BTreeMap::new(),
            reseeds: BTreeMap::new(),
            payloads: None,
            answers: BTreeMap::new(),
        }
    }
    pub fn seed(&self) -> u64 {
        self.seed
    }
    pub fn config(&self) -> &ServiceConfig {
        &self.config
    }
    pub fn set_config(&mut self, config: ServiceConfig) {
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
        self.payloads = payloads;
    }
    pub fn record_reseed(&mut self, at: u64, seed: u64) {
        self.reseeds.insert(at, seed);
    }
    pub fn record_effect(&mut self, at: u64, effect: Effect) {
        self.effects.insert(at, effect);
    }
    pub fn answers(&self) -> &BTreeMap<(u64, u16, u64), Answer> {
        &self.answers
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
        self.answers.insert((at, service, request), answer);
        Ok(())
    }
    pub fn materialize(
        &self,
        factory: &ServiceFactory,
    ) -> Result<RecordedEnv<Box<dyn ServiceHandler>>, ChannelError> {
        let handler = factory(&self.config)?;
        if handler.identity() != self.config.identity
            || handler.configuration() != self.config.configuration
        {
            return Err(ChannelError::Handler(
                "factory returned a different service contract".into(),
            ));
        }
        let mut env = RecordedEnv::new(self.seed, handler);
        env.set_payloads(self.payloads.clone())?;
        for (&(at, service, request), answer) in &self.answers {
            env.record_service_request(at, service, request, answer.clone());
        }
        Ok(env)
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut out = b"HENV".to_vec();
        out.extend(Self::BLOB_VERSION.to_le_bytes());
        out.extend(self.seed.to_le_bytes());
        put(&mut out, &self.config.encode());
        out.extend((self.effects.len() as u32).to_le_bytes());
        for (at, effect) in &self.effects {
            out.extend(at.to_le_bytes());
            put(&mut out, &effect.encode());
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
        out.extend((self.answers.len() as u32).to_le_bytes());
        for ((at, service, request), answer) in &self.answers {
            out.extend(at.to_le_bytes());
            out.extend(service.to_le_bytes());
            out.extend(request.to_le_bytes());
            match answer {
                Answer::Nominal => out.push(0),
                Answer::Data(bytes) => {
                    out.push(1);
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
    pub fn decode(bytes: &[u8]) -> Result<Self, ChannelError> {
        if bytes.len() > MAX_CHANNEL_BYTES {
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
        let mut previous = None;
        for _ in 0..r.count(19)? {
            let key = (r.u64()?, r.u16()?, r.u64()?);
            if previous.is_some_and(|last| last >= key) {
                return Err(ChannelError::Malformed);
            }
            previous = Some(key);
            let answer = match r.take(1)?[0] {
                0 => Answer::Nominal,
                1 => Answer::data(r.bytes()?.to_vec())?,
                _ => return Err(ChannelError::Malformed),
            };
            result.answers.insert(key, answer);
        }
        r.finish()?;
        Ok(result)
    }
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
    #[test]
    fn inputs_round_trip_and_reject_every_truncation() {
        let mut spec = InputSpec::seeded(41);
        spec.record_effect(12, Effect::write_memory(4096, vec![7, 8]).unwrap());
        spec.record_effect(20, Effect::InjectInterrupt { vector: 32 });
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
            identity: b"service".to_vec(),
            configuration: b"options".to_vec(),
        };
        spec.set_config(config.clone());
        assert_eq!(spec.config(), &config);
        spec.record_effect(12, Effect::InjectInterrupt { vector: 32 });
        spec.record_effect(12, Effect::InjectInterrupt { vector: 33 });
        spec.record_reseed(13, 41);
        spec.record_reseed(13, 42);
        assert_eq!(
            spec.effects(),
            &BTreeMap::from([(12, Effect::InjectInterrupt { vector: 33 })])
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
        assert_eq!(InputSpec::decode_snapshot(&bytes).unwrap(), spec);
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
            put(&mut bytes, &Effect::InjectInterrupt { vector: 32 }.encode());
        }
        bytes.extend((reseeds.len() as u32).to_le_bytes());
        for at in reseeds {
            bytes.extend(at.to_le_bytes());
            bytes.extend(43u64.to_le_bytes());
        }
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
        let at = bad_payload_tag.len() - 5;
        bad_payload_tag[at] = 2;
        assert_eq!(
            InputSpec::decode(&bad_payload_tag),
            Err(ChannelError::Malformed)
        );
        let mut bad_answer_tag = ordered_wire(&[], &[], &[key]);
        *bad_answer_tag.last_mut().unwrap() = 2;
        assert_eq!(
            InputSpec::decode(&bad_answer_tag),
            Err(ChannelError::Malformed)
        );
    }
}
