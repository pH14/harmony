// SPDX-License-Identifier: AGPL-3.0-or-later
#![no_std]
#![doc = "Optional fault-package adapters for buggify and network decisions over the generic SDK service channel."]

use harmony_sdk::{Sdk, SdkError, wire};
use hypercall_proto::{Client, ClientError, ProtoError, Transport};

/// Namespace used by the fault package's buggify decision codec.
pub const BUGGIFY_NAMESPACE: u16 = 7;
/// Namespace used by the fault package's network-flow decision codec.
pub const NET_FLOW_NAMESPACE: u16 = 4;
const NET_FLOW_REQUEST_LEN: usize = 18;

/// Fault-package request helpers for a generic hypercall client.
///
/// The generic protocol only transports an opaque SDK opcode-3 request. This
/// trait is where the optional fault package gives two request shapes names and
/// packs their fields; importing the trait is an explicit dependency on those
/// fault semantics.
pub trait FaultClientExt {
    /// The transport error carried by the underlying client.
    type Error;

    /// Ask the fault policy whether a catalog point fires.
    fn buggify_decide(&mut self, point: u32) -> Result<bool, ClientError<Self::Error>>;

    /// Ask the fault policy for an opaque encoded network answer.
    fn net_decide(
        &mut self,
        src: u32,
        dst: u32,
        conn: u64,
        event: u16,
        out: &mut [u8],
    ) -> Result<usize, ClientError<Self::Error>>;
}

impl<T: Transport> FaultClientExt for Client<T> {
    type Error = T::Error;

    fn buggify_decide(&mut self, point: u32) -> Result<bool, ClientError<Self::Error>> {
        let request = point.to_le_bytes();
        let mut response = [0_u8; 1];
        let result =
            self.service_request(BUGGIFY_NAMESPACE, u64::from(point), &request, &mut response)?;
        match result {
            None => Ok(false),
            Some(1) => match response[0] {
                0 => Ok(false),
                1 => Ok(true),
                _ => Err(ClientError::Protocol(ProtoError::BadPayload)),
            },
            Some(_) => Err(ClientError::Protocol(ProtoError::BadPayload)),
        }
    }

    fn net_decide(
        &mut self,
        src: u32,
        dst: u32,
        conn: u64,
        event: u16,
        out: &mut [u8],
    ) -> Result<usize, ClientError<Self::Error>> {
        let mut request = [0_u8; NET_FLOW_REQUEST_LEN];
        request[0..4].copy_from_slice(&src.to_le_bytes());
        request[4..8].copy_from_slice(&dst.to_le_bytes());
        request[8..16].copy_from_slice(&conn.to_le_bytes());
        request[16..18].copy_from_slice(&event.to_le_bytes());
        let result = self.service_request(NET_FLOW_NAMESPACE, conn, &request, out)?;
        match result {
            None => {
                if out.is_empty() {
                    Err(ClientError::Protocol(ProtoError::BufferTooSmall))
                } else {
                    out[0] = 0;
                    Ok(1)
                }
            }
            Some(length) if length > 0 => Ok(length),
            Some(_) => Err(ClientError::Protocol(ProtoError::BadPayload)),
        }
    }
}

/// Fault-package buggify verb for the generic SDK wrapper.
///
/// The base SDK still owns catalog declaration and event framing. This
/// extension owns the request codec and records the returned fire bit in the
/// buggify event namespace, preserving the old guest convenience API without
/// putting fault policy into the generic SDK crate.
pub trait FaultSdkExt {
    /// The transport used by the wrapped SDK client.
    type Transport: Transport;

    /// Resolve and record one fault-package buggify point.
    fn buggify(
        &mut self,
        point: u32,
    ) -> Result<bool, SdkError<<Self::Transport as Transport>::Error>>;
}

impl<T: Transport> FaultSdkExt for Sdk<T> {
    type Transport = T;

    fn buggify(
        &mut self,
        point: u32,
    ) -> Result<bool, SdkError<<Self::Transport as Transport>::Error>> {
        if point > wire::LOCAL_MAX {
            return Err(SdkError::PointIdTooLarge);
        }
        let fired =
            FaultClientExt::buggify_decide(self.client_mut(), point).map_err(SdkError::Client)?;
        self.client_mut()
            .event_emit(wire::event_id(wire::NS_BUGGIFY, point), &[u8::from(fired)])
            .map_err(SdkError::Client)?;
        Ok(fired)
    }
}
