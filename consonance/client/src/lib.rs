// SPDX-License-Identifier: AGPL-3.0-or-later

//! Workload-neutral control clients and SDK observation decoding.

pub mod catalog;

#[cfg(target_os = "linux")]
pub mod watchdog;

#[cfg(all(feature = "in-process", not(miri)))]
pub mod session;

use control_proto::{Caps, ControlError, ExecStatus, Reply, Request};

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

    /// Inject one durable command at the current stopped point and return its
    /// retained state. The request itself never advances the guest.
    pub fn exec_start(
        &mut self,
        cmd: impl Into<String>,
    ) -> Result<Option<ExecStatus>, Box<dyn std::error::Error + Send + Sync>> {
        let reply = self.request(&Request::ExecStart { cmd: cmd.into() })?;
        expect_exec_state(reply, "exec start")
    }

    /// Read the retained durable command without advancing or mutating the
    /// guest. `None` means that this stopped session has no retained command.
    pub fn exec_status(
        &mut self,
    ) -> Result<Option<ExecStatus>, Box<dyn std::error::Error + Send + Sync>> {
        let reply = self.request(&Request::ExecStatus)?;
        expect_exec_state(reply, "exec status")
    }
}

fn expect_exec_state(
    reply: Reply,
    operation: &'static str,
) -> Result<Option<ExecStatus>, Box<dyn std::error::Error + Send + Sync>> {
    match reply {
        Reply::ExecState(status) => Ok(status),
        reply => Err(format!(
            "control request {operation} returned an unexpected reply: {reply:?}"
        )
        .into()),
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

    struct RecordingTransport {
        requests: Vec<Request>,
        replies: VecDeque<Result<Reply, ControlError>>,
    }

    impl Transport for RecordingTransport {
        type Error = std::io::Error;

        fn exchange(
            &mut self,
            request: &Request,
        ) -> Result<Result<Reply, ControlError>, Self::Error> {
            self.requests.push(request.clone());
            self.replies
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

    #[test]
    fn durable_exec_client_methods_send_exact_requests_and_state() {
        let requested = caps();
        let status = ExecStatus {
            id: 3,
            at: control_proto::Moment(17),
            completion: control_proto::ExecCompletion::Pending,
            output: b"partial".to_vec(),
            truncated: false,
        };
        let mut client = Client::connect(
            RecordingTransport {
                requests: Vec::new(),
                replies: VecDeque::from([
                    Ok(Reply::Hello(requested)),
                    Ok(Reply::ExecState(Some(status.clone()))),
                    Ok(Reply::ExecState(None)),
                ]),
            },
            requested,
        )
        .expect("test transport handshake");

        assert_eq!(client.exec_start("printf retained").unwrap(), Some(status));
        assert_eq!(client.exec_status().unwrap(), None);
        assert_eq!(
            client.transport().requests,
            vec![
                Request::Hello(requested),
                Request::ExecStart {
                    cmd: "printf retained".into()
                },
                Request::ExecStatus,
            ]
        );
    }

    #[test]
    fn durable_exec_methods_reject_non_state_replies() {
        let requested = caps();
        let mut client = Client::connect(
            RecordingTransport {
                requests: Vec::new(),
                replies: VecDeque::from([Ok(Reply::Hello(requested)), Ok(Reply::Unit)]),
            },
            requested,
        )
        .expect("test transport handshake");

        let error = client
            .exec_status()
            .expect_err("Unit must not satisfy ExecStatus");
        let message = error.to_string();
        assert!(message.contains("exec status"), "operation lost: {message}");
        assert!(
            message.contains("unexpected reply"),
            "reply validation lost: {message}"
        );
        assert!(message.contains("Unit"), "reply variant lost: {message}");
    }
}
