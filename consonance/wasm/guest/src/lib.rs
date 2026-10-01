// SPDX-License-Identifier: AGPL-3.0-or-later
#![no_std]

use hypercall_proto::{HEADER_LEN, MAX_PAYLOAD, ProtoError, Status, Transport, decode};

pub trait Request {
    fn request(&mut self, operation: u32, input: &[u8], output: &mut [u8]) -> i32;
}

pub struct WasmTransport<R>(pub R);

impl<R: Request> Transport for WasmTransport<R> {
    type Error = ProtoError;

    fn exchange(&mut self, req: &[u8], resp: &mut [u8]) -> Result<usize, Self::Error> {
        let (header, payload) = decode(req)?;
        if header.kind != 1 || header.status != 0 || req.len() != HEADER_LEN + payload.len() {
            return Err(ProtoError::InvalidHeader);
        }
        let output = resp
            .get_mut(HEADER_LEN..)
            .ok_or(ProtoError::BufferTooSmall)?;
        let capacity = output.len().min(MAX_PAYLOAD);
        let result = self.0.request(
            (u32::from(header.service) << 16) | u32::from(header.opcode),
            payload,
            &mut output[..capacity],
        );
        let status = match result {
            0.. => Status::Ok,
            -1 => Status::BadRequest,
            -2 => Status::UnknownService,
            -3 => Status::UnknownOpcode,
            -4 => Status::OutOfRange,
            -5 => Status::Internal,
            _ => return Err(ProtoError::BadPayload),
        };
        let length = if result < 0 { 0 } else { result as usize };
        if length > capacity {
            return Err(ProtoError::BadPayload);
        }
        let mut response_header = [0; HEADER_LEN];
        hypercall_proto::encode_error(
            header.service,
            header.opcode,
            header.seq,
            status,
            &mut response_header,
        );
        response_header[16..20].copy_from_slice(&(length as u32).to_le_bytes());
        resp[..HEADER_LEN].copy_from_slice(&response_header);
        Ok(HEADER_LEN + length)
    }
}

#[cfg(target_arch = "wasm32")]
pub struct ImportedRequest;

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "harmony_v1")]
unsafe extern "C" {
    #[link_name = "request"]
    fn request(
        operation: u32,
        input: *const u8,
        input_len: u32,
        output: *mut u8,
        output_capacity: u32,
    ) -> i32;
}

#[cfg(target_arch = "wasm32")]
impl Request for ImportedRequest {
    fn request(&mut self, operation: u32, input: &[u8], output: &mut [u8]) -> i32 {
        if input.len() > MAX_PAYLOAD || output.len() > MAX_PAYLOAD {
            return -4;
        }
        // SAFETY: the bounded slices remain live and exclusive for the synchronous
        // import; the host validates both linear-memory ranges before any write.
        unsafe {
            request(
                operation,
                input.as_ptr(),
                input.len() as u32,
                output.as_mut_ptr(),
                output.len() as u32,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hypercall_proto::{Client, ServiceId};

    struct Echo(i32);
    impl Request for Echo {
        fn request(&mut self, operation: u32, input: &[u8], output: &mut [u8]) -> i32 {
            assert_eq!(operation, (8 << 16) | 1);
            assert_eq!(input, &[2, 0, 0, 0]);
            output[..2].copy_from_slice(&[0x81, 120]);
            self.0
        }
    }

    #[test]
    fn shared_payload_codec_round_trips_and_propagates_errors() {
        let mut client = Client::new(WasmTransport(Echo(2)));
        let mut action = [0; 2];
        client.payload_fetch(&mut action).unwrap();
        assert_eq!(action, [0x81, 120]);
        let mut client = Client::new(WasmTransport(Echo(-4)));
        assert_eq!(
            client.payload_fetch(&mut action),
            Err(hypercall_proto::ClientError::Status(Status::OutOfRange))
        );
    }

    #[test]
    fn malformed_frames_and_oversized_import_results_are_rejected() {
        let mut request = [0; HEADER_LEN + 4];
        hypercall_proto::encode_request(ServiceId::Payload, 1, 7, &[2, 0, 0, 0], &mut request)
            .unwrap();
        let mut transport = WasmTransport(Echo(i32::MAX));
        assert_eq!(
            transport.exchange(&request, &mut [0; HEADER_LEN + 2]),
            Err(ProtoError::BadPayload)
        );
        request[4] = 2;
        assert_eq!(
            transport.exchange(&request, &mut [0; HEADER_LEN + 2]),
            Err(ProtoError::InvalidHeader)
        );
    }
}
