// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::{
    Answer, DecisionPoint, EnvSpec, Environment, Fault, FaultPolicy, HostFault, Outcome,
    RecordedEnv,
};
use environment::{
    Moment,
    channel::{
        Answer as ChannelAnswer, ChannelError, Effect, Question, ServiceHandler, ServiceResponse,
    },
    input_spec::{InputSpec, ServiceConfig, ServiceFactory, nominal_factory},
};

const IDENTITY: &[u8] = b"harmony-fault-policy-v1";
const BUGGIFY_NAMESPACE: u16 = 7;
const NET_FLOW_NAMESPACE: u16 = 4;
#[derive(Clone)]
struct Handler {
    policy: FaultPolicy,
    config: Vec<u8>,
    env: RecordedEnv,
}
impl Handler {
    fn new(policy: FaultPolicy) -> Self {
        let config = policy.to_bytes();
        let env = EnvSpec::Seeded {
            seed: 0,
            policy: policy.clone(),
        }
        .materialize();
        Self {
            policy,
            config,
            env,
        }
    }
}
impl ServiceHandler for Handler {
    fn identity(&self) -> &[u8] {
        IDENTITY
    }
    fn configuration(&self) -> &[u8] {
        &self.config
    }
    fn reseed(&mut self, seed: u64) {
        self.env = EnvSpec::Seeded {
            seed,
            policy: self.policy.clone(),
        }
        .materialize();
    }
    fn respond(
        &mut self,
        _moment: Moment,
        question: &Question,
    ) -> Result<ServiceResponse, ChannelError> {
        let payload = question.payload();
        let point = match question.service() {
            BUGGIFY_NAMESPACE if payload.len() == 4 => DecisionPoint::Buggify {
                point: u32::from_le_bytes(payload.try_into().map_err(|_| ChannelError::Malformed)?),
            },
            NET_FLOW_NAMESPACE if payload.len() == 18 => {
                if payload[16..18] != [0, 0] {
                    return Err(ChannelError::Handler("unsupported flow event".into()));
                }
                DecisionPoint::NetFlow {
                    src: crate::NodeId(u32::from_le_bytes(
                        payload[0..4]
                            .try_into()
                            .map_err(|_| ChannelError::Malformed)?,
                    )),
                    dst: crate::NodeId(u32::from_le_bytes(
                        payload[4..8]
                            .try_into()
                            .map_err(|_| ChannelError::Malformed)?,
                    )),
                    conn: crate::ConnId(u64::from_le_bytes(
                        payload[8..16]
                            .try_into()
                            .map_err(|_| ChannelError::Malformed)?,
                    )),
                    event: crate::FlowEvent::Open,
                }
            }
            NET_FLOW_NAMESPACE | BUGGIFY_NAMESPACE => return Err(ChannelError::Malformed),
            _ => return Ok(ServiceResponse::Answered(ChannelAnswer::Nominal)),
        };
        let Outcome::Resolved(answer) = self.env.decide(&point) else {
            return Ok(ServiceResponse::External);
        };
        let bytes = if question.service() == BUGGIFY_NAMESPACE {
            vec![u8::from(matches!(
                answer,
                Answer::Fault(Fault::BuggifyFire)
            ))]
        } else {
            answer.encode()
        };
        Ok(ServiceResponse::Answered(ChannelAnswer::data(bytes)?))
    }
    fn snapshot_state(&self) -> Result<Vec<u8>, ChannelError> {
        Ok(self.env.stream_state().to_vec())
    }
    fn restore_state(&mut self, state: &[u8]) -> Result<(), ChannelError> {
        let bytes: &[u8; 16] = state.try_into().map_err(|_| ChannelError::Malformed)?;
        self.env.restore_stream_state(bytes);
        Ok(())
    }
    fn clone_box(&self) -> Box<dyn ServiceHandler> {
        Box::new(self.clone())
    }
}
pub fn service_factory() -> ServiceFactory {
    let nominal = nominal_factory();
    std::sync::Arc::new(move |config| {
        if config.identity != IDENTITY {
            return nominal(config);
        }
        let policy =
            FaultPolicy::from_bytes(&config.configuration).map_err(|_| ChannelError::Malformed)?;
        if !policy.is_enforceable_only() {
            return Err(ChannelError::Handler(
                "legacy service policy has no installed enforcement".into(),
            ));
        }
        Ok(Box::new(Handler::new(policy)))
    })
}
pub fn translate(spec: &EnvSpec) -> Result<InputSpec, ChannelError> {
    if !spec.policy().is_enforceable_only()
        || matches!(spec, EnvSpec::Recorded { standing, .. } if !standing.is_empty())
    {
        return Err(ChannelError::Handler(
            "legacy reproducer requires unavailable fault enforcement".into(),
        ));
    }
    let mut result = InputSpec::seeded(spec.seed());
    if spec.policy() != &FaultPolicy::none() {
        result.set_config(ServiceConfig {
            identity: IDENTITY.to_vec(),
            configuration: spec.policy().to_bytes(),
        });
    }
    for (&at, action) in spec.overrides() {
        let Some(fault) = action.host_fault() else {
            return Err(ChannelError::Handler(
                "legacy pinned guest answers require a request identity".into(),
            ));
        };
        result.record_effect(at, effect(&fault)?);
    }
    for (&at, &seed) in spec.reseeds() {
        result.record_reseed(at, seed);
    }
    result.set_payloads(spec.payloads().map(<[Vec<u8>]>::to_vec));
    Ok(result)
}
pub fn effect(fault: &HostFault) -> Result<Effect, ChannelError> {
    match fault {
        HostFault::CorruptMemory { gpa, mask } => Ok(Effect::XorMemory {
            gpa: *gpa,
            bytes: mask.0.to_le_bytes().to_vec(),
        }),
        HostFault::InjectInterrupt { vector } => Ok(Effect::InjectInterrupt { vector: *vector }),
        HostFault::SkewTime(_) | HostFault::SetClockRate(_) => Err(ChannelError::Handler(
            "historical clock fault has no supported enforcement".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_service_stream_and_snapshot_continuation_are_preserved() {
        let mut policy = FaultPolicy::none();
        policy.set_buggify_default(1, 2).unwrap();
        policy
            .set_class(crate::DecisionClass::NetFlow, 1, 2, &[Fault::NetReset])
            .unwrap();
        let spec = EnvSpec::Seeded { seed: 91, policy };
        let mut old = spec.materialize();
        let mut new = translate(&spec)
            .unwrap()
            .materialize(&service_factory())
            .unwrap();
        let mut flow = Vec::new();
        flow.extend(1_u32.to_le_bytes());
        flow.extend(2_u32.to_le_bytes());
        flow.extend(7_u64.to_le_bytes());
        flow.extend(0_u16.to_le_bytes());
        let net = Question::new(4, flow).unwrap();
        let bug = Question::new(7, 42_u32.to_le_bytes().to_vec()).unwrap();
        for _ in 0..8 {
            let Outcome::Resolved(expected) = old.decide(&DecisionPoint::NetFlow {
                src: crate::NodeId(1),
                dst: crate::NodeId(2),
                conn: crate::ConnId(7),
                event: crate::FlowEvent::Open,
            }) else {
                panic!("seeded response");
            };
            assert_eq!(
                new.decide(&net).unwrap(),
                ServiceResponse::Answered(ChannelAnswer::Data(expected.encode()))
            );
            let Outcome::Resolved(expected) = old.decide(&DecisionPoint::Buggify { point: 42 })
            else {
                panic!("seeded response");
            };
            assert_eq!(
                new.decide(&bug).unwrap(),
                ServiceResponse::Answered(ChannelAnswer::Data(vec![u8::from(matches!(
                    expected,
                    Answer::Fault(Fault::BuggifyFire)
                ))]))
            );
        }
        let saved = new.snapshot_state().unwrap();
        let expected = new.decide(&bug).unwrap();
        let mut restored = translate(&spec)
            .unwrap()
            .materialize(&service_factory())
            .unwrap();
        saved.restore_into(&mut restored).unwrap();
        assert_eq!(restored.decide(&bug).unwrap(), expected);
    }
    #[test]
    fn machine_effects_preserve_xor_and_reject_unimplemented_clock_faults() {
        assert_eq!(
            effect(&HostFault::CorruptMemory {
                gpa: 4096,
                mask: crate::BitMask(0x102)
            })
            .unwrap(),
            Effect::XorMemory {
                gpa: 4096,
                bytes: 0x102_u64.to_le_bytes().to_vec()
            }
        );
        assert!(effect(&HostFault::SkewTime(crate::Span(1))).is_err());
    }
}
