// SPDX-License-Identifier: AGPL-3.0-or-later

use hypercall_proto::decode;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kind {
    Event,
    Coverage,
    Service,
    Payload,
    Pvclock,
    Other,
    Unknown,
    Invalid,
    Unreadable,
    ResponseUnknown,
}

impl Kind {
    const ALL: [Self; 10] = [
        Self::Event,
        Self::Coverage,
        Self::Service,
        Self::Payload,
        Self::Pvclock,
        Self::Other,
        Self::Unknown,
        Self::Invalid,
        Self::Unreadable,
        Self::ResponseUnknown,
    ];
    fn name(self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::Coverage => "coverage",
            Self::Service => "service",
            Self::Payload => "payload",
            Self::Pvclock => "pvclock",
            Self::Other => "other",
            Self::Unknown => "unknown",
            Self::Invalid => "invalid",
            Self::Unreadable => "unreadable",
            Self::ResponseUnknown => "response_unclassified",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Shape {
    namespace: u8,
    len: usize,
    first: Option<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Attribution {
    kind: Kind,
    shape: Option<Shape>,
}

impl Attribution {
    pub(crate) const fn kind(kind: Kind) -> Self {
        Self { kind, shape: None }
    }
    pub(crate) fn request(bytes: &[u8]) -> Self {
        let Ok((header, payload)) = decode(bytes) else {
            return Self::kind(Kind::Invalid);
        };
        if !header.is_request() {
            return Self::kind(Kind::Invalid);
        }
        let kind = match (header.service, header.opcode) {
            (4, 1) => Kind::Event,
            (6, 2) => Kind::Coverage,
            (6, 3) => Kind::Service,
            (8, 1) => Kind::Payload,
            (7, 1) => Kind::Pvclock,
            (1..=8, _) => Kind::Other,
            _ => Kind::Unknown,
        };
        let shape = if kind == Kind::Event && payload.len() >= 4 {
            Some(Shape {
                namespace: payload[3],
                len: payload.len() - 4,
                first: payload.get(4).copied(),
            })
        } else {
            None
        };
        Self { kind, shape }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Callback {
    pub(crate) calls: u64,
    pub(crate) errors: u64,
    pub(crate) nanos: u64,
}

impl Callback {
    pub(crate) fn measured(present: bool, failed: bool, nanos: u64) -> Self {
        if present {
            Self {
                calls: 1,
                errors: u64::from(failed),
                nanos,
            }
        } else {
            Self::default()
        }
    }
    fn merge(&mut self, rhs: Self) {
        self.calls = self.calls.saturating_add(rhs.calls);
        self.errors = self.errors.saturating_add(rhs.errors);
        self.nanos = self.nanos.saturating_add(rhs.nanos);
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Work {
    requests: u64,
    deferred: u64,
    response: Callback,
}

impl Work {
    fn merge(&mut self, rhs: Self) {
        self.requests = self.requests.saturating_add(rhs.requests);
        self.deferred = self.deferred.saturating_add(rhs.deferred);
        self.response.merge(rhs.response);
    }
    fn counters(self, prefix: &str, out: &mut Vec<(String, u64)>) {
        for (name, value) in [
            ("requests", self.requests),
            ("deferred", self.deferred),
            ("response_guard_calls", self.response.calls),
            ("response_guard_errors", self.response.errors),
            ("response_guard_callback_ns", self.response.nanos),
        ] {
            out.push((format!("{prefix}.{name}"), value));
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SdkDispatchTelemetry {
    bins: [Work; 10],
    shapes: [Option<(Shape, Work)>; 32],
    overflow: Work,
}

impl SdkDispatchTelemetry {
    fn add(&mut self, at: Attribution, work: Work) {
        self.bins[at.kind as usize].merge(work);
        if let Some(shape) = at.shape {
            self.add_shape(shape, work);
        }
    }
    fn add_shape(&mut self, shape: Shape, work: Work) {
        if let Some((_, total)) = self
            .shapes
            .iter_mut()
            .flatten()
            .find(|(key, _)| *key == shape)
        {
            total.merge(work);
        } else if let Some(slot) = self.shapes.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some((shape, work));
        } else {
            self.overflow.merge(work);
        }
    }
    pub(crate) fn request(&mut self, at: Attribution, deferred: bool) {
        self.add(
            at,
            Work {
                requests: 1,
                deferred: u64::from(deferred),
                ..Work::default()
            },
        );
    }
    pub(crate) fn response(&mut self, at: Attribution, callback: Callback) {
        self.add(
            at,
            Work {
                response: callback,
                ..Work::default()
            },
        );
    }
    pub fn merge(&mut self, rhs: &Self) {
        for (left, right) in self.bins.iter_mut().zip(rhs.bins) {
            left.merge(right);
        }
        for (shape, work) in rhs.shapes.iter().flatten() {
            self.add_shape(*shape, *work);
        }
        self.overflow.merge(rhs.overflow);
    }
    pub fn counters(&self) -> Vec<(String, u64)> {
        let mut out = Vec::new();
        for kind in Kind::ALL {
            let work = self.bins[kind as usize];
            if work != Work::default() {
                work.counters(&format!("sdk_dispatch.{}", kind.name()), &mut out);
            }
        }
        for (shape, work) in self.shapes.iter().flatten() {
            let first = shape
                .first
                .map_or_else(|| "none".to_owned(), |first| first.to_string());
            work.counters(
                &format!(
                    "sdk_dispatch.event_shape.{}.{}.{}",
                    shape.namespace, shape.len, first
                ),
                &mut out,
            );
        }
        out.push((
            "sdk_dispatch.event_shape_slots".to_owned(),
            self.shapes.iter().flatten().count() as u64,
        ));
        self.overflow
            .counters("sdk_dispatch.event_shape_overflow", &mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hypercall_proto::{ServiceId, encode_request};

    fn frame(service: ServiceId, opcode: u16, payload: &[u8]) -> Vec<u8> {
        let mut bytes = [0; 4096];
        let len = encode_request(service, opcode, 7, payload, &mut bytes).unwrap();
        bytes[..len].to_vec()
    }

    #[test]
    fn sdk_dispatch_classification_and_callback_numbers_are_exact() {
        for (service, opcode, kind) in [
            (ServiceId::Event, 1, Kind::Event),
            (ServiceId::Sdk, 2, Kind::Coverage),
            (ServiceId::Sdk, 3, Kind::Service),
            (ServiceId::Payload, 1, Kind::Payload),
            (ServiceId::Pvclock, 1, Kind::Pvclock),
            (ServiceId::Event, 99, Kind::Other),
        ] {
            assert_eq!(
                Attribution::request(&frame(service, opcode, &[])).kind,
                kind
            );
        }
        assert_eq!(Attribution::request(&[]).kind, Kind::Invalid);
        let at = Attribution::request(&frame(ServiceId::Event, 1, &[1, 0, 0, 2, 0, 9]));
        assert_eq!(
            at.shape,
            Some(Shape {
                namespace: 2,
                len: 2,
                first: Some(0)
            })
        );
        let mut work = SdkDispatchTelemetry::default();
        work.request(at, true);
        work.response(at, Callback::measured(true, true, 23));
        work.response(
            Attribution::kind(Kind::Service),
            Callback::measured(false, true, 999),
        );
        assert_eq!(
            work.bins[Kind::Event as usize],
            Work {
                requests: 1,
                deferred: 1,
                response: Callback {
                    calls: 1,
                    errors: 1,
                    nanos: 23
                }
            }
        );
        assert_eq!(
            work.bins[Kind::Service as usize].response,
            Callback::default()
        );
        let counters = work.counters();
        assert!(counters.contains(&(
            "sdk_dispatch.event_shape.2.2.0.response_guard_errors".into(),
            1
        )));
    }

    #[test]
    fn sdk_dispatch_witness_capacity_merge_and_overflow_preserve_coverage() {
        let mut first = SdkDispatchTelemetry::default();
        for namespace in 0..33 {
            first.request(
                Attribution {
                    kind: Kind::Event,
                    shape: Some(Shape {
                        namespace,
                        len: 9,
                        first: Some(0),
                    }),
                },
                false,
            );
        }
        let known = Attribution {
            kind: Kind::Event,
            shape: Some(Shape {
                namespace: 0,
                len: 9,
                first: Some(0),
            }),
        };
        first.response(known, Callback::measured(true, true, 11));
        assert_eq!(first.shapes.iter().flatten().count(), 32);
        assert_eq!(first.overflow.requests, 1);
        let before = first.clone();
        first.merge(&before);
        let mut covered = first.overflow;
        for (_, work) in first.shapes.iter().flatten() {
            covered.merge(*work);
        }
        assert_eq!(covered, first.bins[Kind::Event as usize]);
        assert_eq!(covered.requests, 66);
        assert_eq!(covered.response.errors, 2);
        assert_eq!(first.shapes[0].unwrap().0, known.shape.unwrap());
        let mut other = SdkDispatchTelemetry::default();
        other.request(
            Attribution {
                kind: Kind::Event,
                shape: Some(Shape {
                    namespace: 100,
                    len: 9,
                    first: Some(0),
                }),
            },
            false,
        );
        first.merge(&other);
        assert_eq!(first.overflow.requests, 3);
        assert_eq!(first.shapes.iter().flatten().count(), 32);
        let mut covered = first.overflow;
        for (_, work) in first.shapes.iter().flatten() {
            covered.merge(*work);
        }
        assert_eq!(covered, first.bins[Kind::Event as usize]);
        assert_eq!(covered.requests, 67);
        let mut saturated = Callback {
            calls: u64::MAX,
            errors: u64::MAX,
            nanos: u64::MAX,
        };
        saturated.merge(Callback {
            calls: 7,
            errors: 3,
            nanos: 11,
        });
        assert_eq!(
            saturated,
            Callback {
                calls: u64::MAX,
                errors: u64::MAX,
                nanos: u64::MAX
            }
        );
    }
}
