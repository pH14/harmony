// SPDX-License-Identifier: AGPL-3.0-or-later

//! Workload-neutral control clients and SDK observation decoding.

pub mod catalog;

#[cfg(all(feature = "in-process", not(miri)))]
pub mod session;

use control_proto::{Caps, ControlError, Reply, Request};

/// One synchronous control exchange. Implementations own transport/session state.
pub trait Transport {
    type Error: std::error::Error + Send + Sync + 'static;
    fn exchange(&mut self, request: &Request) -> Result<Result<Reply, ControlError>, Self::Error>;
}

/// A negotiated client; construction establishes the protocol before other verbs.
pub struct Client<T> {
    transport: T,
    capabilities: Caps,
}

impl<T: Transport> Client<T> {
    pub fn connect(
        mut transport: T,
        requested: Caps,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let reply = transport
            .exchange(&Request::Hello(requested))?
            .map_err(|error| format!("hello rejected: {error:?}"))?;
        let Reply::Hello(capabilities) = reply else {
            return Err("hello returned an unexpected reply".into());
        };
        if capabilities != requested {
            return Err("control capabilities differ from requested contract".into());
        }
        Ok(Self {
            transport,
            capabilities,
        })
    }

    /// Access implementation-specific snapshot and diagnostic capabilities.
    pub fn transport(&self) -> &T {
        &self.transport
    }
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    pub fn capabilities(&self) -> &Caps {
        &self.capabilities
    }

    pub fn request(
        &mut self,
        request: &Request,
    ) -> Result<Reply, Box<dyn std::error::Error + Send + Sync>> {
        if matches!(request, Request::Hello(_)) {
            return Err("client already negotiated".into());
        }
        self.transport
            .exchange(request)?
            .map_err(|error| format!("control request rejected: {error:?}").into())
    }
}

#[cfg(feature = "in-process")]
impl<B: vmm_backend::Backend<A: vmm_core::vendor::Vendor>> Transport
    for vmm_core::control::ControlServer<B>
{
    type Error = vmm_core::control::ServeError;
    fn exchange(&mut self, request: &Request) -> Result<Result<Reply, ControlError>, Self::Error> {
        self.handle(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    struct FakeTransport(VecDeque<Result<Reply, ControlError>>);
    impl Transport for FakeTransport {
        type Error = std::io::Error;
        fn exchange(&mut self, _: &Request) -> Result<Result<Reply, ControlError>, Self::Error> {
            self.0
                .pop_front()
                .ok_or_else(|| std::io::Error::other("connection closed"))
        }
    }
    fn caps() -> Caps {
        Caps {
            protocol_version: control_proto::APP_PROTOCOL_VERSION,
            env_version_min: 5,
            env_version_max: 5,
            coverage: Default::default(),
            flags: Default::default(),
        }
    }
    #[test]
    fn handshake_rejects_changed_contract_and_unexpected_reply() {
        let mut incompatible = caps();
        incompatible.env_version_max += 1;
        for reply in [Reply::Hello(incompatible), Reply::Unit] {
            assert!(Client::connect(FakeTransport([Ok(reply)].into()), caps()).is_err());
        }
    }
    #[test]
    fn request_errors_and_closed_transport_remain_errors() {
        let mut client = Client::connect(
            FakeTransport(
                [
                    Ok(Reply::Hello(caps())),
                    Err(ControlError::Unsupported),
                    Ok(Reply::Unit),
                ]
                .into(),
            ),
            caps(),
        )
        .unwrap();
        assert!(client.request(&Request::Hello(caps())).is_err());
        assert!(client.request(&Request::RecordedEnv).is_err());
        assert_eq!(client.request(&Request::RecordedEnv).unwrap(), Reply::Unit);
        assert!(client.request(&Request::RecordedEnv).is_err());
    }
}
