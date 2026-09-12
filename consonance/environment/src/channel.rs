// SPDX-License-Identifier: AGPL-3.0-or-later

//! Fault-agnostic service questions, answers, and deterministic extension state.
//!
//! This module is the transition boundary for the generic Consonance core. A
//! question is an opaque, bounded service payload; the core does not classify
//! it as a network, block, process, or application fault. Workload packages
//! may decode [`Answer::Data`] and install a handler, while the default
//! [`NominalHandler`] answers every question nominally.

use std::{collections::BTreeMap, fmt};

use thiserror::Error;

use crate::Moment;

/// Maximum bytes carried by one question, answer, or handler state field.
pub const MAX_CHANNEL_BYTES: usize = 1 << 20;

const SNAPSHOT_MAGIC: u32 = u32::from_le_bytes(*b"EVC1");
const SNAPSHOT_VERSION: u16 = 1;
const MAX_RECORDED_STATE_BYTES: usize = 8 * MAX_CHANNEL_BYTES;

/// Generic service ids reserved for the deterministic core supplies.
/// Workload handlers may use any other service namespace.
pub const SERVICE_ENTROPY: u16 = 1;
pub const SERVICE_PAYLOAD: u16 = 2;
pub const SERVICE_SCHEDULER: u16 = 3;

/// A bounded, service-owned request. The generic core preserves the service
/// number and bytes without interpreting either field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Question {
    request_id: u64,
    service: u16,
    payload: Vec<u8>,
}

impl Question {
    /// Construct a question, rejecting an unbounded guest payload.
    pub fn new(service: u16, payload: Vec<u8>) -> Result<Self, ChannelError> {
        Self::with_request_id(0, service, payload)
    }

    /// Construct a question with an explicit request identity. The identity
    /// disambiguates two decisions that surface at the same deterministic
    /// moment.
    pub fn with_request_id(
        request_id: u64,
        service: u16,
        payload: Vec<u8>,
    ) -> Result<Self, ChannelError> {
        check_len(payload.len())?;
        Ok(Self {
            request_id,
            service,
            payload,
        })
    }

    /// Construct an entropy supply request for the deterministic core.
    pub fn entropy(bytes: u32) -> Result<Self, ChannelError> {
        Self::new(SERVICE_ENTROPY, bytes.to_le_bytes().to_vec())
    }

    /// Construct a scheduler request for the deterministic core.
    pub fn scheduler(ready: u32) -> Result<Self, ChannelError> {
        Self::new(SERVICE_SCHEDULER, ready.to_le_bytes().to_vec())
    }

    /// The stable request identity used with a recorded override.
    #[must_use]
    pub fn request_id(&self) -> u64 {
        self.request_id
    }

    /// The service namespace chosen by the workload or guest extension.
    #[must_use]
    pub fn service(&self) -> u16 {
        self.service
    }

    /// The service-owned request bytes.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
}

/// A generic response. `Data` is intentionally opaque: a fault package can
/// place its encoded answer there, while an ordinary Consonance run never
/// needs to link a fault catalog.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Answer {
    /// The service proceeded without a workload-specific payload.
    Nominal,
    /// Bounded bytes interpreted by the service that issued the question.
    Data(Vec<u8>),
}

impl Answer {
    /// Construct a bounded data response.
    pub fn data(payload: Vec<u8>) -> Result<Self, ChannelError> {
        check_len(payload.len())?;
        Ok(Self::Data(payload))
    }

    /// Framed response bytes: tag zero is nominal, tag one precedes opaque data.
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Nominal => vec![0],
            Self::Data(bytes) => {
                let mut out = vec![1];
                out.extend(bytes);
                out
            }
        }
    }
    /// Decode a response whose total length is supplied by its transport frame.
    pub fn decode(bytes: &[u8]) -> Result<Self, ChannelError> {
        match bytes {
            [0] => Ok(Self::Nominal),
            [1, payload @ ..] if payload.len() <= MAX_CHANNEL_BYTES => Self::data(payload.to_vec()),
            _ => Err(ChannelError::Malformed),
        }
    }

    /// Borrow the opaque payload, if this is a data response.
    #[must_use]
    pub fn payload(&self) -> Option<&[u8]> {
        match self {
            Self::Nominal => None,
            Self::Data(payload) => Some(payload),
        }
    }
}

/// Whether a question was answered by the local backing or needs an external
/// service decision. External handling is explicit so an ordinary run cannot
/// silently invent a fault answer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceResponse {
    /// The handler supplied the complete response.
    Answered(Answer),
    /// The caller must obtain a response from an external service.
    External,
}

/// A mechanical operation the execution core can apply without understanding
/// why a workload requested it. Fault packages translate their own catalog
/// into these effects outside this module.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Effect {
    /// Write a bounded byte sequence into guest physical memory.
    WriteMemory { gpa: u64, bytes: Vec<u8> },
    /// XOR a bounded byte sequence into guest physical memory at execution time.
    XorMemory { gpa: u64, bytes: Vec<u8> },
    /// Deliver one architecture-defined interrupt identity.
    InjectInterrupt { vector: u32 },
}

impl Effect {
    /// Construct a memory write, rejecting an oversized operation.
    pub fn write_memory(gpa: u64, bytes: Vec<u8>) -> Result<Self, ChannelError> {
        check_len(bytes.len())?;
        Ok(Self::WriteMemory { gpa, bytes })
    }

    /// Construct a memory XOR, rejecting an oversized operation.
    pub fn xor_memory(gpa: u64, bytes: Vec<u8>) -> Result<Self, ChannelError> {
        check_len(bytes.len())?;
        Ok(Self::XorMemory { gpa, bytes })
    }

    /// Encode the mechanical effect with stable tags and bounded lengths.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::WriteMemory { gpa, bytes } => {
                out.push(1);
                put_u64(&mut out, *gpa);
                put_bytes(&mut out, bytes);
            }
            Self::XorMemory { gpa, bytes } => {
                out.push(3);
                put_u64(&mut out, *gpa);
                put_bytes(&mut out, bytes);
            }
            Self::InjectInterrupt { vector } => {
                out.push(2);
                put_u32(&mut out, *vector);
            }
        }
        out
    }

    /// Decode an effect strictly and without allocation from an unbounded
    /// length field.
    pub fn decode(bytes: &[u8]) -> Result<Self, ChannelError> {
        let mut reader = Reader::new(bytes);
        let effect = match reader.u8()? {
            1 => Self::write_memory(reader.u64()?, reader.bytes()?.to_vec())?,
            3 => Self::xor_memory(reader.u64()?, reader.bytes()?.to_vec())?,
            2 => Self::InjectInterrupt {
                vector: reader.u32()?,
            },
            _ => return Err(ChannelError::Malformed),
        };
        reader.finish()?;
        Ok(effect)
    }
}

/// A deterministic set of primitive effects. It is separate from
/// [`RecordedEnv`] because a control-input reproducer owns the static schedule;
/// the environment snapshot only owns dynamic service state.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EffectSchedule {
    effects: BTreeMap<Moment, Vec<Effect>>,
}

impl EffectSchedule {
    /// Add an effect and canonicalize all effects at that moment.
    pub fn insert(&mut self, moment: Moment, effect: Effect) {
        let effects = self.effects.entry(moment).or_default();
        effects.push(effect);
        effects.sort();
        effects.dedup();
    }

    /// Effects due at one timeline position.
    #[must_use]
    pub fn at(&self, moment: Moment) -> &[Effect] {
        self.effects.get(&moment).map_or(&[], Vec::as_slice)
    }

    /// Canonical schedule entries.
    pub fn iter(&self) -> impl Iterator<Item = (Moment, &[Effect])> {
        self.effects
            .iter()
            .map(|(moment, effects)| (*moment, effects.as_slice()))
    }
}

/// Errors from bounded channel construction or extension-state restoration.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ChannelError {
    /// A payload or state field exceeds [`MAX_CHANNEL_BYTES`].
    #[error("channel payload exceeds the bounded maximum")]
    TooLarge,
    /// Bytes do not form the canonical representation of the expected value.
    #[error("malformed channel state")]
    Malformed,
    /// A handler cannot restore the supplied opaque state.
    #[error("service handler state was rejected: {0}")]
    Handler(String),
}

/// A deterministic service extension. Identity and configuration are recorded
/// separately from dynamic state, so restoring a snapshot onto a different
/// handler or image fails closed.
pub trait ServiceHandler: Send {
    /// Stable handler implementation identity, including its protocol version.
    fn identity(&self) -> &[u8];

    /// Stable handler configuration or workload identity bytes.
    fn configuration(&self) -> &[u8];

    /// Answer a question locally or request an external response. The moment is
    /// the timeline position the question surfaced at, so a handler whose answer
    /// depends on the guest's position in virtual time does not have to track it.
    fn respond(
        &mut self,
        moment: Moment,
        question: &Question,
    ) -> Result<ServiceResponse, ChannelError>;

    /// Capture dynamic extension state for a machine snapshot.
    fn snapshot_state(&self) -> Result<Vec<u8>, ChannelError>;

    /// Restore dynamic extension state after identity/configuration checks.
    fn restore_state(&mut self, state: &[u8]) -> Result<(), ChannelError>;

    /// Reset handler-owned deterministic streams for a new branch seed.
    /// Handlers without seeded state can retain the default no-op behavior.
    fn reseed(&mut self, _seed: u64) {}

    /// Clone the handler for a new worker or replay session.
    fn clone_box(&self) -> Box<dyn ServiceHandler>;
}

impl ServiceHandler for Box<dyn ServiceHandler> {
    fn identity(&self) -> &[u8] {
        (**self).identity()
    }

    fn configuration(&self) -> &[u8] {
        (**self).configuration()
    }

    fn respond(
        &mut self,
        moment: Moment,
        question: &Question,
    ) -> Result<ServiceResponse, ChannelError> {
        (**self).respond(moment, question)
    }

    fn snapshot_state(&self) -> Result<Vec<u8>, ChannelError> {
        (**self).snapshot_state()
    }

    fn restore_state(&mut self, state: &[u8]) -> Result<(), ChannelError> {
        (**self).restore_state(state)
    }

    fn reseed(&mut self, seed: u64) {
        (**self).reseed(seed);
    }

    fn clone_box(&self) -> Box<dyn ServiceHandler> {
        (**self).clone_box()
    }
}

impl Clone for Box<dyn ServiceHandler> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

/// Factory used when a worker reconstructs a handler from a portable snapshot.
pub trait ServiceHandlerFactory {
    /// Concrete handler produced by this factory.
    type Handler: ServiceHandler;

    /// Build a handler after checking its recorded implementation and config.
    fn build(&self, identity: &[u8], configuration: &[u8]) -> Result<Self::Handler, ChannelError>;
}

/// Portable handler identity/configuration/dynamic state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandlerSnapshot {
    identity: Vec<u8>,
    configuration: Vec<u8>,
    state: Vec<u8>,
}

impl HandlerSnapshot {
    /// Capture one handler's complete extension state.
    pub fn capture(handler: &dyn ServiceHandler) -> Result<Self, ChannelError> {
        let identity = handler.identity().to_vec();
        let configuration = handler.configuration().to_vec();
        check_len(identity.len())?;
        check_len(configuration.len())?;
        let state = handler.snapshot_state()?;
        check_len(state.len())?;
        Ok(Self {
            identity,
            configuration,
            state,
        })
    }

    /// Rebuild a handler from the recorded identity/configuration and restore
    /// its dynamic state.
    pub fn restore<F: ServiceHandlerFactory>(
        &self,
        factory: &F,
    ) -> Result<F::Handler, ChannelError> {
        let mut handler = factory.build(&self.identity, &self.configuration)?;
        handler.restore_state(&self.state)?;
        Ok(handler)
    }

    /// Handler implementation identity.
    #[must_use]
    pub fn identity(&self) -> &[u8] {
        &self.identity
    }

    /// Handler configuration or workload identity.
    #[must_use]
    pub fn configuration(&self) -> &[u8] {
        &self.configuration
    }

    /// Dynamic handler bytes.
    #[must_use]
    pub fn state(&self) -> &[u8] {
        &self.state
    }
}

/// Snapshot of the generic environment's dynamic state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordedState {
    stream_state: u64,
    moment: Moment,
    overrides: BTreeMap<DecisionKey, Answer>,
    payloads: Option<Vec<Vec<u8>>>,
    handler: HandlerSnapshot,
}

impl RecordedState {
    /// Recorded host responses, including future responses staged at this seal.
    pub fn answers(&self) -> impl Iterator<Item = ((Moment, u16, u64), &Answer)> {
        self.overrides
            .iter()
            .map(|(key, answer)| ((key.moment, key.service, key.request_id), answer))
    }

    /// Capture a generic environment state, including the handler extension.
    pub fn capture<H: ServiceHandler>(environment: &RecordedEnv<H>) -> Result<Self, ChannelError> {
        for answer in environment.overrides.values() {
            if let Answer::Data(bytes) = answer {
                check_len(bytes.len())?;
            }
        }
        let state = Self {
            stream_state: environment.stream_state,
            moment: environment.moment,
            overrides: environment.overrides.clone(),
            payloads: environment.remaining_payloads(),
            handler: HandlerSnapshot::capture(&environment.handler)?,
        };
        if state.encoded_len() > MAX_RECORDED_STATE_BYTES {
            return Err(ChannelError::TooLarge);
        }
        Ok(state)
    }

    fn encoded_len(&self) -> usize {
        let mut size = 39usize;
        for answer in self.overrides.values() {
            size = size.saturating_add(19);
            if let Answer::Data(bytes) = answer {
                size = size.saturating_add(4).saturating_add(bytes.len());
            }
        }
        if let Some(entries) = &self.payloads {
            size = size.saturating_add(4);
            for entry in entries {
                size = size.saturating_add(4).saturating_add(entry.len());
            }
        }
        size.saturating_add(self.handler.identity.len())
            .saturating_add(self.handler.configuration.len())
            .saturating_add(self.handler.state.len())
    }

    /// Encode a portable, canonical state blob.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_u32(&mut out, SNAPSHOT_MAGIC);
        put_u16(&mut out, SNAPSHOT_VERSION);
        put_u64(&mut out, self.stream_state);
        put_u64(&mut out, self.moment);
        put_len(&mut out, self.overrides.len());
        for (key, answer) in &self.overrides {
            put_u64(&mut out, key.moment);
            put_u16(&mut out, key.service);
            put_u64(&mut out, key.request_id);
            put_answer(&mut out, answer);
        }
        match &self.payloads {
            None => out.push(0),
            Some(entries) => {
                out.push(1);
                put_len(&mut out, entries.len());
                for entry in entries {
                    put_bytes(&mut out, entry);
                }
            }
        }
        put_bytes(&mut out, &self.handler.identity);
        put_bytes(&mut out, &self.handler.configuration);
        put_bytes(&mut out, &self.handler.state);
        out
    }

    /// Remaining ordered payload suffix captured in this state.
    #[must_use]
    pub fn remaining_payloads(&self) -> Option<Vec<Vec<u8>>> {
        self.payloads.clone()
    }

    /// Decode a portable state blob strictly.
    pub fn decode(bytes: &[u8]) -> Result<Self, ChannelError> {
        if bytes.len() > MAX_RECORDED_STATE_BYTES {
            return Err(ChannelError::TooLarge);
        }
        let mut reader = Reader::new(bytes);
        if reader.u32()? != SNAPSHOT_MAGIC || reader.u16()? != SNAPSHOT_VERSION {
            return Err(ChannelError::Malformed);
        }
        let stream_state = reader.u64()?;
        let moment = reader.u64()?;
        let override_count = reader.count(19)?;
        let mut overrides = BTreeMap::new();
        let mut previous = None;
        for _ in 0..override_count {
            let key = DecisionKey {
                moment: reader.u64()?,
                service: reader.u16()?,
                request_id: reader.u64()?,
            };
            if previous.is_some_and(|previous| previous >= key) {
                return Err(ChannelError::Malformed);
            }
            previous = Some(key);
            overrides.insert(key, read_answer(&mut reader)?);
        }
        let payloads = match reader.u8()? {
            0 => None,
            1 => {
                let count = reader.count(4)?;
                let mut entries = Vec::with_capacity(count);
                for _ in 0..count {
                    entries.push(reader.bytes()?.to_vec());
                }
                Some(entries)
            }
            _ => return Err(ChannelError::Malformed),
        };
        let handler = HandlerSnapshot {
            identity: reader.bytes()?.to_vec(),
            configuration: reader.bytes()?.to_vec(),
            state: reader.bytes()?.to_vec(),
        };
        reader.finish()?;
        Ok(Self {
            stream_state,
            moment,
            overrides,
            payloads,
            handler,
        })
    }

    /// Restore a state onto a matching handler-backed environment.
    pub fn restore_into<H: ServiceHandler + Clone>(
        &self,
        environment: &mut RecordedEnv<H>,
    ) -> Result<(), ChannelError> {
        if environment.handler.identity() != self.handler.identity
            || environment.handler.configuration() != self.handler.configuration
        {
            return Err(ChannelError::Handler(
                "handler identity or configuration differs from the snapshot".to_owned(),
            ));
        }
        let mut handler = environment.handler.clone();
        handler.restore_state(&self.handler.state)?;
        environment.handler = handler;
        environment.stream_state = self.stream_state;
        environment.moment = self.moment;
        environment.overrides = self.overrides.clone();
        environment.restore_payloads(self.payloads.clone());
        Ok(())
    }
}

/// A no-fault handler used by ordinary Consonance execution.
#[derive(Clone, Debug, Default)]
pub struct NominalHandler;

const NOMINAL_IDENTITY: &[u8] = b"consonance.nominal-handler.v1";

impl ServiceHandler for NominalHandler {
    fn identity(&self) -> &[u8] {
        NOMINAL_IDENTITY
    }

    fn configuration(&self) -> &[u8] {
        &[]
    }

    fn respond(
        &mut self,
        _moment: Moment,
        _question: &Question,
    ) -> Result<ServiceResponse, ChannelError> {
        Ok(ServiceResponse::Answered(Answer::Nominal))
    }

    fn snapshot_state(&self) -> Result<Vec<u8>, ChannelError> {
        Ok(Vec::new())
    }

    fn restore_state(&mut self, state: &[u8]) -> Result<(), ChannelError> {
        if state.is_empty() {
            Ok(())
        } else {
            Err(ChannelError::Handler(
                "nominal handler has no dynamic state".to_owned(),
            ))
        }
    }

    fn clone_box(&self) -> Box<dyn ServiceHandler> {
        Box::new(self.clone())
    }
}

/// Deterministic generic backing with opaque service-handler extension state.
pub struct RecordedEnv<H: ServiceHandler> {
    stream_state: u64,
    overrides: BTreeMap<DecisionKey, Answer>,
    moment: Moment,
    payloads: Option<Vec<Vec<u8>>>,
    payload_cursor: usize,
    handler: H,
}

impl<H> Clone for RecordedEnv<H>
where
    H: ServiceHandler + Clone,
{
    fn clone(&self) -> Self {
        Self {
            stream_state: self.stream_state,
            overrides: self.overrides.clone(),
            moment: self.moment,
            payloads: self.payloads.clone(),
            payload_cursor: self.payload_cursor,
            handler: self.handler.clone(),
        }
    }
}

impl<H: ServiceHandler> fmt::Debug for RecordedEnv<H> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RecordedEnv")
            .field("stream_state", &self.stream_state)
            .field("overrides", &self.overrides)
            .field("moment", &self.moment)
            .field("payload_cursor", &self.payload_cursor)
            .field("handler_identity", &self.handler.identity())
            .finish_non_exhaustive()
    }
}

impl<H: ServiceHandler> RecordedEnv<H> {
    /// Construct a deterministic environment with no overrides or payload tape.
    pub fn new(seed: u64, handler: H) -> Self {
        let mut environment = Self {
            stream_state: normalize_seed(seed),
            overrides: BTreeMap::new(),
            moment: 0,
            payloads: None,
            payload_cursor: 0,
            handler,
        };
        environment.handler.reseed(seed);
        environment
    }

    /// Set the timeline position used for the next override lookup.
    pub fn set_moment(&mut self, moment: Moment) {
        self.moment = moment;
    }

    /// Reset the deterministic core and handler streams for a new branch seed.
    pub fn reseed(&mut self, seed: u64) {
        self.stream_state = normalize_seed(seed);
        self.handler.reseed(seed);
    }

    /// Add or replace an opaque response at a deterministic timeline position.
    pub fn record(&mut self, moment: Moment, answer: Answer) {
        self.record_service_request(moment, 0, 0, answer);
    }

    /// Add or replace an opaque response for an explicit request identity.
    pub fn record_request(&mut self, moment: Moment, request_id: u64, answer: Answer) {
        self.record_service_request(moment, 0, request_id, answer);
    }

    /// Add or replace an opaque response keyed by timeline, service namespace,
    /// and request identity. The service component prevents independent
    /// namespaces from aliasing a response at one deterministic moment.
    pub fn record_service_request(
        &mut self,
        moment: Moment,
        service: u16,
        request_id: u64,
        answer: Answer,
    ) {
        self.overrides.insert(
            DecisionKey {
                moment,
                service,
                request_id,
            },
            answer,
        );
    }

    /// Record a response for the exact question identity.
    pub fn record_question(&mut self, moment: Moment, question: &Question, answer: Answer) {
        self.record_service_request(moment, question.service(), question.request_id(), answer);
    }

    /// Offer an ordered payload tape. `Some(empty)` remains distinct from no tape.
    pub fn set_payloads(&mut self, payloads: Option<Vec<Vec<u8>>>) -> Result<(), ChannelError> {
        if let Some(entries) = &payloads {
            for entry in entries {
                check_len(entry.len())?;
            }
        }
        self.payloads = payloads;
        self.payload_cursor = 0;
        Ok(())
    }

    /// Whether an ordered payload service was offered, including an exhausted tape.
    #[must_use]
    pub fn payload_configured(&self) -> bool {
        self.payloads.is_some()
    }

    /// Consume one exact-length payload entry.
    pub fn pull_payload(&mut self, bytes: usize) -> Result<Option<Vec<u8>>, ChannelError> {
        check_len(bytes)?;
        let Some(entries) = self.payloads.as_ref() else {
            return Ok(None);
        };
        let Some(entry) = entries.get(self.payload_cursor) else {
            return Ok(None);
        };
        if entry.len() != bytes {
            return Err(ChannelError::Handler(
                "payload tape entry length does not match the request".to_owned(),
            ));
        }
        self.payload_cursor += 1;
        Ok(Some(entry.clone()))
    }

    /// Answer from an override first, otherwise delegate to the service handler.
    pub fn decide(&mut self, question: &Question) -> Result<ServiceResponse, ChannelError>
    where
        H: Clone,
    {
        let key = DecisionKey {
            moment: self.moment,
            service: question.service(),
            request_id: question.request_id(),
        };
        if let Some(answer) = self.overrides.get(&key) {
            return Ok(ServiceResponse::Answered(answer.clone()));
        }
        if let Some(answer) = self.decide_core_supply(question)? {
            return Ok(ServiceResponse::Answered(answer));
        }
        let mut candidate = self.handler.clone();
        let response = candidate.respond(self.moment, question)?;
        if let ServiceResponse::Answered(answer) = &response {
            if let Answer::Data(bytes) = answer {
                check_len(bytes.len())?;
            }
            self.handler = candidate;
        }
        Ok(response)
    }

    /// Current handler identity and configuration.
    pub fn handler(&self) -> &H {
        &self.handler
    }

    /// Capture all dynamic state needed to resume this environment.
    pub fn snapshot_state(&self) -> Result<RecordedState, ChannelError> {
        RecordedState::capture(self)
    }

    /// Remaining ordered payload tape, preserving unavailable vs exhausted.
    pub fn remaining_payloads(&self) -> Option<Vec<Vec<u8>>> {
        self.payloads
            .as_ref()
            .map(|entries| entries[self.payload_cursor..].to_vec())
    }

    fn restore_payloads(&mut self, payloads: Option<Vec<Vec<u8>>>) {
        self.payloads = payloads;
        self.payload_cursor = 0;
    }

    fn decide_core_supply(&mut self, question: &Question) -> Result<Option<Answer>, ChannelError> {
        let service = question.service();
        if service != SERVICE_ENTROPY && service != SERVICE_SCHEDULER {
            return Ok(None);
        }
        let bytes = question.payload();
        let request = bytes
            .try_into()
            .map_err(|_| ChannelError::Malformed)
            .map(u32::from_le_bytes)?;
        match service {
            SERVICE_ENTROPY => {
                let length = usize::try_from(request).map_err(|_| ChannelError::TooLarge)?;
                check_len(length)?;
                let mut output = Vec::with_capacity(length);
                while output.len() < length {
                    output.extend_from_slice(&next_stream(&mut self.stream_state).to_le_bytes());
                }
                output.truncate(length);
                Ok(Some(Answer::Data(output)))
            }
            SERVICE_SCHEDULER => {
                if request == 0 {
                    return Err(ChannelError::Malformed);
                }
                let selected = (next_stream(&mut self.stream_state) % u64::from(request)) as u32;
                Ok(Some(Answer::Data(selected.to_le_bytes().to_vec())))
            }
            _ => unreachable!(),
        }
    }
}

impl RecordedEnv<NominalHandler> {
    /// Construct a no-fault environment with the nominal handler.
    pub fn nominal(seed: u64) -> Self {
        Self::new(seed, NominalHandler)
    }
}

/// Construct a handler from a portable snapshot through a workload-owned factory.
pub fn restore_handler<F: ServiceHandlerFactory>(
    snapshot: &HandlerSnapshot,
    factory: &F,
) -> Result<F::Handler, ChannelError> {
    snapshot.restore(factory)
}

fn normalize_seed(seed: u64) -> u64 {
    if seed == 0 {
        0x9E37_79B9_7F4A_7C15
    } else {
        seed
    }
}

fn next_stream(state: &mut u64) -> u64 {
    *state ^= *state >> 12;
    *state ^= *state << 25;
    *state ^= *state >> 27;
    state.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

fn check_len(len: usize) -> Result<(), ChannelError> {
    (len <= MAX_CHANNEL_BYTES)
        .then_some(())
        .ok_or(ChannelError::TooLarge)
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_len(out: &mut Vec<u8>, len: usize) {
    put_u32(out, u32::try_from(len).unwrap_or(u32::MAX));
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    put_len(out, bytes.len());
    out.extend_from_slice(bytes);
}

fn put_answer(out: &mut Vec<u8>, answer: &Answer) {
    match answer {
        Answer::Nominal => out.push(0),
        Answer::Data(bytes) => {
            out.push(1);
            put_bytes(out, bytes);
        }
    }
}

fn read_answer(reader: &mut Reader<'_>) -> Result<Answer, ChannelError> {
    match reader.u8()? {
        0 => Ok(Answer::Nominal),
        1 => Ok(Answer::Data(reader.bytes()?.to_vec())),
        _ => Err(ChannelError::Malformed),
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct DecisionKey {
    moment: Moment,
    service: u16,
    request_id: u64,
}

struct Reader<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], ChannelError> {
        let end = self
            .cursor
            .checked_add(count)
            .ok_or(ChannelError::Malformed)?;
        let bytes = self
            .bytes
            .get(self.cursor..end)
            .ok_or(ChannelError::Malformed)?;
        self.cursor = end;
        Ok(bytes)
    }

    fn u8(&mut self) -> Result<u8, ChannelError> {
        Ok(*self.take(1)?.first().ok_or(ChannelError::Malformed)?)
    }

    fn u16(&mut self) -> Result<u16, ChannelError> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().unwrap_or([0; 2]),
        ))
    }

    fn u32(&mut self) -> Result<u32, ChannelError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().unwrap_or([0; 4]),
        ))
    }

    fn u64(&mut self) -> Result<u64, ChannelError> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().unwrap_or([0; 8]),
        ))
    }

    fn len(&mut self) -> Result<usize, ChannelError> {
        let len = usize::try_from(self.u32()?).map_err(|_| ChannelError::Malformed)?;
        check_len(len)?;
        Ok(len)
    }

    fn count(&mut self, minimum_bytes: usize) -> Result<usize, ChannelError> {
        let count = self.len()?;
        if count > self.remaining() / minimum_bytes {
            return Err(ChannelError::Malformed);
        }
        Ok(count)
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.cursor)
    }

    fn bytes(&mut self) -> Result<&'a [u8], ChannelError> {
        let len = self.len()?;
        self.take(len)
    }

    fn finish(self) -> Result<(), ChannelError> {
        self.cursor
            .eq(&self.bytes.len())
            .then_some(())
            .ok_or(ChannelError::Malformed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug)]
    struct Handler {
        counter: u64,
    }

    impl ServiceHandler for Handler {
        fn identity(&self) -> &[u8] {
            b"test-handler-v1"
        }

        fn configuration(&self) -> &[u8] {
            b"test-config"
        }

        fn respond(
            &mut self,
            _moment: Moment,
            question: &Question,
        ) -> Result<ServiceResponse, ChannelError> {
            self.counter += 1;
            if question.service() == u16::MAX {
                return Ok(ServiceResponse::Answered(Answer::Data(vec![
                    0;
                    MAX_CHANNEL_BYTES
                        + 1
                ])));
            }
            Ok(ServiceResponse::Answered(Answer::data(
                self.counter.to_le_bytes().to_vec(),
            )?))
        }

        fn snapshot_state(&self) -> Result<Vec<u8>, ChannelError> {
            Ok(self.counter.to_le_bytes().to_vec())
        }

        fn restore_state(&mut self, state: &[u8]) -> Result<(), ChannelError> {
            if state.len() != 8 {
                self.counter = 0;
                return Err(ChannelError::Malformed);
            }
            let bytes: [u8; 8] = state.try_into().map_err(|_| ChannelError::Malformed)?;
            self.counter = u64::from_le_bytes(bytes);
            Ok(())
        }

        fn clone_box(&self) -> Box<dyn ServiceHandler> {
            Box::new(self.clone())
        }
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct ForwardingHandler {
        seed: u64,
        state: u8,
    }

    impl ServiceHandler for ForwardingHandler {
        fn identity(&self) -> &[u8] {
            b"forwarding-handler-v1"
        }

        fn configuration(&self) -> &[u8] {
            b"forwarding-config"
        }

        fn respond(
            &mut self,
            _moment: Moment,
            _question: &Question,
        ) -> Result<ServiceResponse, ChannelError> {
            self.state = self.state.wrapping_add(1);
            Ok(ServiceResponse::Answered(Answer::Nominal))
        }

        fn snapshot_state(&self) -> Result<Vec<u8>, ChannelError> {
            Ok(vec![self.state, self.seed as u8])
        }

        fn restore_state(&mut self, state: &[u8]) -> Result<(), ChannelError> {
            let [value, seed] = state else {
                return Err(ChannelError::Malformed);
            };
            self.state = *value;
            self.seed = u64::from(*seed);
            Ok(())
        }

        fn reseed(&mut self, seed: u64) {
            self.seed = seed;
        }

        fn clone_box(&self) -> Box<dyn ServiceHandler> {
            Box::new(self.clone())
        }
    }

    struct Factory;

    impl ServiceHandlerFactory for Factory {
        type Handler = Handler;

        fn build(
            &self,
            identity: &[u8],
            configuration: &[u8],
        ) -> Result<Self::Handler, ChannelError> {
            (identity == b"test-handler-v1" && configuration == b"test-config")
                .then_some(Handler { counter: 0 })
                .ok_or_else(|| ChannelError::Handler("wrong handler identity".to_owned()))
        }
    }

    #[test]
    fn opaque_handler_state_round_trips_and_factory_checks_identity() {
        let question = Question::new(7, vec![1, 2]).unwrap();
        let mut env = RecordedEnv::new(9, Handler { counter: 0 });
        assert_eq!(
            env.decide(&question).unwrap(),
            ServiceResponse::Answered(Answer::data(vec![1, 0, 0, 0, 0, 0, 0, 0]).unwrap())
        );
        let snapshot = env.snapshot_state().unwrap();
        let bytes = snapshot.encode();
        let decoded = RecordedState::decode(&bytes).unwrap();
        let mut restored = RecordedEnv::new(99, Handler { counter: 0 });
        decoded.restore_into(&mut restored).unwrap();
        assert_eq!(
            restored.decide(&question).unwrap(),
            ServiceResponse::Answered(Answer::data(vec![2, 0, 0, 0, 0, 0, 0, 0]).unwrap())
        );
        let handler = restore_handler(&decoded.handler, &Factory).unwrap();
        assert_eq!(handler.counter, 1);
    }

    #[test]
    fn a_handler_sees_the_moment_the_question_surfaced_at() {
        #[derive(Clone, Default)]
        struct MomentEcho(Vec<Moment>);

        impl ServiceHandler for MomentEcho {
            fn identity(&self) -> &[u8] {
                b"moment-echo-v1"
            }

            fn configuration(&self) -> &[u8] {
                &[]
            }

            fn respond(
                &mut self,
                moment: Moment,
                _question: &Question,
            ) -> Result<ServiceResponse, ChannelError> {
                self.0.push(moment);
                Ok(ServiceResponse::Answered(Answer::data(
                    moment.to_le_bytes().to_vec(),
                )?))
            }

            fn snapshot_state(&self) -> Result<Vec<u8>, ChannelError> {
                Ok(Vec::new())
            }

            fn restore_state(&mut self, _state: &[u8]) -> Result<(), ChannelError> {
                Ok(())
            }

            fn clone_box(&self) -> Box<dyn ServiceHandler> {
                Box::new(self.clone())
            }
        }

        let question = Question::new(7, vec![]).unwrap();
        let mut env = RecordedEnv::new(1, MomentEcho::default());
        for moment in [11, 11, 25] {
            env.set_moment(moment);
            assert_eq!(
                env.decide(&question).unwrap(),
                ServiceResponse::Answered(Answer::Data(moment.to_le_bytes().to_vec()))
            );
        }
        assert_eq!(env.handler().0, vec![11, 11, 25]);
    }

    #[test]
    fn failed_handler_restore_does_not_partially_mutate_environment() {
        let source = RecordedEnv::new(1, Handler { counter: 3 });
        let mut snapshot = source.snapshot_state().unwrap();
        snapshot.handler.state = vec![0xff];
        let mut target = RecordedEnv::new(9, Handler { counter: 8 });
        assert!(target.decide(&Question::new(7, vec![]).unwrap()).is_ok());
        let before = target.handler.counter;
        assert!(snapshot.restore_into(&mut target).is_err());
        assert_eq!(target.handler.counter, before);
    }

    #[test]
    fn malformed_effects_and_state_are_rejected() {
        assert_eq!(Effect::decode(&[9]), Err(ChannelError::Malformed));
        let xor = Effect::xor_memory(0x4000, vec![1, 2, 3]).unwrap();
        assert_eq!(Effect::decode(&xor.encode()), Ok(xor));
        assert_eq!(RecordedState::decode(b"EVC1"), Err(ChannelError::Malformed));
    }

    #[test]
    fn mechanical_effect_schedule_is_canonical_and_fault_agnostic() {
        let mut schedule = EffectSchedule::default();
        schedule.insert(7, Effect::InjectInterrupt { vector: 9 });
        schedule.insert(7, Effect::InjectInterrupt { vector: 3 });
        schedule.insert(7, Effect::InjectInterrupt { vector: 9 });
        assert_eq!(
            schedule.at(7),
            &[
                Effect::InjectInterrupt { vector: 3 },
                Effect::InjectInterrupt { vector: 9 },
            ]
        );
    }

    #[test]
    fn request_identity_keeps_same_moment_overrides_distinct() {
        let mut env = RecordedEnv::new(1, Handler { counter: 0 });
        env.record_service_request(4, 7, 11, Answer::Data(vec![7]));
        env.set_moment(4);
        let first = Question::with_request_id(11, 7, vec![]).unwrap();
        let second = Question::with_request_id(12, 7, vec![]).unwrap();
        assert_eq!(
            env.decide(&first).unwrap(),
            ServiceResponse::Answered(Answer::Data(vec![7]))
        );
        assert_eq!(
            env.decide(&second).unwrap(),
            ServiceResponse::Answered(Answer::Data(vec![1, 0, 0, 0, 0, 0, 0, 0]))
        );
    }

    #[test]
    fn aggregate_snapshot_limit_matches_decoder_for_responses_and_payloads() {
        let mut env = RecordedEnv::nominal(17);
        for id in 0..7 {
            env.record_request(id, id, Answer::Data(vec![1; MAX_CHANNEL_BYTES]));
        }
        let state = env.snapshot_state().unwrap();
        let bytes = state.encode();
        assert_eq!(state.encoded_len(), bytes.len());
        assert_eq!(RecordedState::decode(&bytes).unwrap(), state);
        env.record_request(7, 7, Answer::Data(vec![1; MAX_CHANNEL_BYTES]));
        assert_eq!(env.snapshot_state(), Err(ChannelError::TooLarge));

        let mut env = RecordedEnv::nominal(17);
        env.set_payloads(Some(vec![vec![1; MAX_CHANNEL_BYTES]; 7]))
            .unwrap();
        let state = env.snapshot_state().unwrap();
        assert_eq!(state.encoded_len(), state.encode().len());
        assert_eq!(RecordedState::decode(&state.encode()).unwrap(), state);
        env.set_payloads(Some(vec![vec![1; MAX_CHANNEL_BYTES]; 8]))
            .unwrap();
        assert_eq!(env.snapshot_state(), Err(ChannelError::TooLarge));
    }

    #[test]
    fn snapshot_aggregate_boundary_includes_all_framing_bytes() {
        let mut env = RecordedEnv::nominal(17);
        env.set_payloads(Some(vec![Vec::new(); 8])).unwrap();
        let overhead = env.snapshot_state().unwrap().encode().len();
        let mut entries = vec![vec![0; MAX_CHANNEL_BYTES]; 7];
        entries.push(vec![0; MAX_CHANNEL_BYTES - overhead]);
        env.set_payloads(Some(entries.clone())).unwrap();
        let state = env.snapshot_state().unwrap();
        assert_eq!(state.encode().len(), MAX_RECORDED_STATE_BYTES);
        assert_eq!(RecordedState::decode(&state.encode()).unwrap(), state);
        entries.last_mut().unwrap().push(0);
        env.set_payloads(Some(entries)).unwrap();
        assert_eq!(env.snapshot_state(), Err(ChannelError::TooLarge));
    }

    #[test]
    fn invalid_handler_answer_does_not_commit_handler_state() {
        let mut env = RecordedEnv::new(17, Handler { counter: 0 });
        let question = Question::new(u16::MAX, vec![]).unwrap();
        assert!(env.decide(&question).is_err());
        assert_eq!(env.handler().counter, 0);
    }

    #[test]
    fn core_supply_is_deterministic_and_snapshot_restores_overrides() {
        let mut env = RecordedEnv::nominal(17);
        let question = Question::entropy(9).unwrap();
        let expected = env.decide(&question).unwrap();
        env.record_question(3, &question, Answer::Data(vec![42]));
        env.set_moment(3);
        let snapshot = env.snapshot_state().unwrap();
        let decoded = RecordedState::decode(&snapshot.encode()).unwrap();
        let mut restored = RecordedEnv::nominal(99);
        decoded.restore_into(&mut restored).unwrap();
        assert_eq!(
            restored.decide(&question),
            Ok(ServiceResponse::Answered(Answer::Data(vec![42])))
        );

        let mut replay = RecordedEnv::nominal(17);
        assert_eq!(replay.decide(&question), Ok(expected));
    }

    #[test]
    fn question_and_answer_codecs_cover_accessors_and_limits() {
        let question = Question::with_request_id(23, 41, vec![1, 2]).unwrap();
        assert_eq!(question.request_id(), 23);
        assert_eq!(question.service(), 41);
        assert_eq!(question.payload(), &[1, 2]);
        assert_eq!(
            Question::new(41, vec![0; MAX_CHANNEL_BYTES])
                .unwrap()
                .payload()
                .len(),
            MAX_CHANNEL_BYTES
        );
        assert_eq!(
            Question::new(41, vec![0; MAX_CHANNEL_BYTES + 1]),
            Err(ChannelError::TooLarge)
        );

        let nominal = Answer::Nominal;
        assert_eq!(nominal.encode(), vec![0]);
        assert_eq!(nominal.payload(), None);
        assert_eq!(Answer::decode(&[0]), Ok(nominal.clone()));
        assert_eq!(Answer::decode(&[2]), Err(ChannelError::Malformed));

        let data = Answer::data(vec![3, 4]).unwrap();
        assert_eq!(data.encode(), vec![1, 3, 4]);
        assert_eq!(data.payload(), Some(&[3, 4][..]));
        assert_eq!(Answer::decode(&data.encode()), Ok(data));
        assert_eq!(
            Answer::decode(&[1; MAX_CHANNEL_BYTES + 2]),
            Err(ChannelError::Malformed)
        );
        assert_eq!(
            Answer::data(vec![0; MAX_CHANNEL_BYTES])
                .unwrap()
                .payload()
                .unwrap()
                .len(),
            MAX_CHANNEL_BYTES
        );
        assert_eq!(
            Answer::data(vec![0; MAX_CHANNEL_BYTES + 1]),
            Err(ChannelError::TooLarge)
        );
    }

    #[test]
    fn effects_and_schedule_round_trip_all_variants() {
        let effects = [
            Effect::write_memory(0x1000, vec![1, 2]).unwrap(),
            Effect::xor_memory(0x2000, vec![3, 4]).unwrap(),
            Effect::InjectInterrupt { vector: 17 },
        ];
        for effect in &effects {
            assert_eq!(Effect::decode(&effect.encode()), Ok(effect.clone()));
        }
        assert_eq!(
            Effect::write_memory(0, vec![0; MAX_CHANNEL_BYTES + 1]),
            Err(ChannelError::TooLarge)
        );
        assert_eq!(
            Effect::xor_memory(0, vec![0; MAX_CHANNEL_BYTES + 1]),
            Err(ChannelError::TooLarge)
        );
        let mut schedule = EffectSchedule::default();
        schedule.insert(9, effects[2].clone());
        schedule.insert(3, effects[0].clone());
        schedule.insert(9, effects[1].clone());
        assert_eq!(
            schedule.iter().collect::<Vec<_>>(),
            vec![
                (3, &[effects[0].clone()][..]),
                (9, &[effects[1].clone(), effects[2].clone()][..])
            ]
        );
    }

    #[test]
    fn boxed_handlers_and_handler_snapshot_forward_every_operation() {
        let mut boxed: Box<dyn ServiceHandler> = Box::new(ForwardingHandler { seed: 1, state: 2 });
        assert_eq!(boxed.identity(), b"forwarding-handler-v1");
        assert_eq!(boxed.configuration(), b"forwarding-config");
        assert_eq!(boxed.snapshot_state().unwrap(), vec![2, 1]);
        boxed.reseed(9);
        assert_eq!(boxed.snapshot_state().unwrap(), vec![2, 9]);
        boxed.restore_state(&[7, 8]).unwrap();
        assert_eq!(boxed.snapshot_state().unwrap(), vec![7, 8]);
        let cloned = boxed.clone_box();
        assert_eq!(cloned.snapshot_state().unwrap(), vec![7, 8]);

        let snapshot = HandlerSnapshot::capture(&*boxed).unwrap();
        assert_eq!(snapshot.identity(), b"forwarding-handler-v1");
        assert_eq!(snapshot.configuration(), b"forwarding-config");
        assert_eq!(snapshot.state(), &[7, 8]);
    }

    #[test]
    fn recorded_state_exposes_answers_and_remaining_payloads() {
        let mut env = RecordedEnv::nominal(3);
        env.record_service_request(4, 8, 12, Answer::Data(vec![5]));
        env.set_payloads(Some(vec![vec![1], vec![2]])).unwrap();
        assert!(env.payload_configured());
        assert_eq!(env.pull_payload(1).unwrap(), Some(vec![1]));
        let state = env.snapshot_state().unwrap();
        assert_eq!(state.remaining_payloads(), Some(vec![vec![2]]));
        assert_eq!(
            state.answers().collect::<Vec<_>>(),
            vec![((4, 8, 12), &Answer::Data(vec![5]))]
        );

        let mut restored = RecordedEnv::nominal(99);
        state.restore_into(&mut restored).unwrap();
        assert_eq!(restored.remaining_payloads(), Some(vec![vec![2]]));
        restored.set_moment(4);
        let question = Question::with_request_id(12, 8, vec![]).unwrap();
        assert_eq!(
            restored.decide(&question),
            Ok(ServiceResponse::Answered(Answer::Data(vec![5])))
        );
    }

    #[test]
    fn payload_tape_is_bounded_exact_and_transactional() {
        let mut absent = RecordedEnv::nominal(1);
        assert!(!absent.payload_configured());
        assert_eq!(absent.pull_payload(1).unwrap(), None);
        assert_eq!(
            absent.pull_payload(MAX_CHANNEL_BYTES + 1),
            Err(ChannelError::TooLarge)
        );

        let mut env = RecordedEnv::nominal(1);
        env.set_payloads(Some(vec![vec![9], vec![8, 7]])).unwrap();
        assert!(matches!(env.pull_payload(2), Err(ChannelError::Handler(_))));
        assert_eq!(env.pull_payload(1).unwrap(), Some(vec![9]));
        assert_eq!(env.pull_payload(2).unwrap(), Some(vec![8, 7]));
        assert_eq!(env.pull_payload(0).unwrap(), None);
        assert_eq!(env.remaining_payloads(), Some(vec![]));
        env.set_payloads(None).unwrap();
        assert!(!env.payload_configured());
        assert_eq!(
            env.set_payloads(Some(vec![vec![0; MAX_CHANNEL_BYTES + 1]])),
            Err(ChannelError::TooLarge)
        );
    }

    #[test]
    fn records_and_reseeds_change_the_selected_decision_stream() {
        let mut env = RecordedEnv::new(1, Handler { counter: 0 });
        env.record(4, Answer::Data(vec![3]));
        env.record_request(4, 9, Answer::Data(vec![4]));
        env.set_moment(4);
        assert_eq!(
            env.decide(&Question::new(0, vec![]).unwrap()),
            Ok(ServiceResponse::Answered(Answer::Data(vec![3])))
        );
        assert_eq!(
            env.decide(&Question::with_request_id(9, 0, vec![]).unwrap()),
            Ok(ServiceResponse::Answered(Answer::Data(vec![4])))
        );

        let mut seeded = RecordedEnv::nominal(1);
        let first = seeded.decide(&Question::entropy(8).unwrap()).unwrap();
        assert_eq!(
            first,
            ServiceResponse::Answered(Answer::Data(5180492295206395165u64.to_le_bytes().to_vec()))
        );
        seeded.reseed(2);
        let second = seeded.decide(&Question::entropy(8).unwrap()).unwrap();
        assert_eq!(
            second,
            ServiceResponse::Answered(Answer::Data(10360984590412790330u64.to_le_bytes().to_vec()))
        );
    }

    #[test]
    fn core_supply_validates_services_and_stream_boundaries() {
        let mut env = RecordedEnv::nominal(1);
        assert_eq!(
            env.decide(&Question::scheduler(7).unwrap()),
            Ok(ServiceResponse::Answered(Answer::Data(
                5u32.to_le_bytes().to_vec()
            )))
        );
        assert_eq!(
            env.decide(&Question::scheduler(0).unwrap()),
            Err(ChannelError::Malformed)
        );
        assert_eq!(
            env.decide(&Question::new(SERVICE_SCHEDULER, vec![1]).unwrap()),
            Err(ChannelError::Malformed)
        );

        let mut stream = RecordedEnv::nominal(1);
        let _ = stream.decide(&Question::entropy(9).unwrap()).unwrap();
        let next = stream.decide(&Question::entropy(1).unwrap()).unwrap();
        assert_eq!(next, ServiceResponse::Answered(Answer::Data(vec![0x57])));

        let mut exact_stream = RecordedEnv::nominal(1);
        let _ = exact_stream.decide(&Question::entropy(8).unwrap()).unwrap();
        let next = exact_stream.decide(&Question::entropy(1).unwrap()).unwrap();
        assert_eq!(next, ServiceResponse::Answered(Answer::Data(vec![0x1d])));

        let mut xor_stream = RecordedEnv::nominal(0x1234_5678_9abc_def0);
        assert_eq!(
            xor_stream.decide(&Question::entropy(8).unwrap()),
            Ok(ServiceResponse::Answered(Answer::Data(
                13257192714554721081u64.to_le_bytes().to_vec()
            )))
        );

        let mut handler = RecordedEnv::new(1, Handler { counter: 0 });
        let response = handler.decide(&Question::new(99, vec![]).unwrap()).unwrap();
        assert_eq!(
            response,
            ServiceResponse::Answered(Answer::Data(vec![1, 0, 0, 0, 0, 0, 0, 0]))
        );
    }

    #[test]
    fn reader_and_snapshot_decoders_enforce_canonical_boundaries() {
        let mut exact = Reader::new(&[1, 0, 0, 0, 0, 0]);
        assert_eq!(exact.count(1).unwrap(), 1);
        let mut too_many = Reader::new(&[2, 0, 0, 0, 0]);
        assert_eq!(too_many.count(1), Err(ChannelError::Malformed));
        let mut exact_width = Reader::new(&[2, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(exact_width.count(2).unwrap(), 2);
        let mut wrong_width = Reader::new(&[3, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(wrong_width.count(2), Err(ChannelError::Malformed));
        assert_eq!(Reader::new(&[]).finish(), Ok(()));
        assert_eq!(Reader::new(&[0]).finish(), Err(ChannelError::Malformed));
        assert_eq!(read_answer(&mut Reader::new(&[0])), Ok(Answer::Nominal));
        assert_eq!(
            read_answer(&mut Reader::new(&[1, 1, 0, 0, 0, 9])),
            Ok(Answer::Data(vec![9]))
        );
        assert_eq!(
            read_answer(&mut Reader::new(&[9])),
            Err(ChannelError::Malformed)
        );

        assert_ne!(
            RecordedState::decode(&vec![0; MAX_RECORDED_STATE_BYTES]),
            Err(ChannelError::TooLarge)
        );
        assert_eq!(
            RecordedState::decode(&vec![0; MAX_RECORDED_STATE_BYTES + 1]),
            Err(ChannelError::TooLarge)
        );
    }

    #[test]
    fn restore_requires_both_handler_identity_and_configuration() {
        let state = RecordedEnv::new(1, ForwardingHandler { seed: 1, state: 2 })
            .snapshot_state()
            .unwrap();
        let mut wrong_identity = state.clone();
        wrong_identity.handler.identity = b"other".to_vec();
        let mut target = RecordedEnv::new(2, ForwardingHandler { seed: 2, state: 3 });
        assert!(wrong_identity.restore_into(&mut target).is_err());
        let mut wrong_config = state;
        wrong_config.handler.configuration = b"other".to_vec();
        assert!(wrong_config.restore_into(&mut target).is_err());
    }

    #[test]
    fn nominal_handler_contract_is_stable_and_stateless() {
        let handler = NominalHandler;
        assert_eq!(handler.identity(), b"consonance.nominal-handler.v1");
        assert_eq!(handler.configuration(), b"");
        assert_eq!(handler.snapshot_state(), Ok(Vec::new()));
        let mut restored = NominalHandler;
        assert_eq!(restored.restore_state(&[]), Ok(()));
        assert_eq!(
            restored.restore_state(&[1]),
            Err(ChannelError::Handler(
                "nominal handler has no dynamic state".to_owned()
            ))
        );
    }

    #[test]
    fn debug_output_contains_recorded_environment_fields() {
        let env = RecordedEnv::nominal(7);
        let debug = format!("{env:?}");
        assert!(debug.contains("stream_state"));
        assert!(debug.contains("handler_identity"));
    }
}
