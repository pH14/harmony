// SPDX-License-Identifier: AGPL-3.0-or-later
pub const NAMESPACE: u16 = 10;
pub const OUTPUT_EVENT: u32 = 0x00fe_0001;
pub const STATUS_EVENT: u32 = 0x00fe_0002;
pub const MAX_DATA: usize = 3000;
pub const MAX_SCRIPT: usize = 1024 * 1024;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Operation {
    Append = 1,
    Execute = 2,
    Shell = 3,
    Input = 4,
    Close = 5,
    Resize = 6,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Message {
    pub id: u64,
    pub operation: Operation,
    pub data: Vec<u8>,
}
impl Message {
    pub fn encode(&self) -> Result<Vec<u8>, &'static str> {
        if self.id == 0 || self.data.len() > MAX_DATA {
            return Err("invalid debug message size or sequence");
        }
        let mut bytes = self.id.to_le_bytes().to_vec();
        bytes.push(self.operation as u8);
        bytes.extend_from_slice(&self.data);
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() < 9 || bytes.len() > 9 + MAX_DATA {
            return Err("invalid debug message length");
        }
        let id = u64::from_le_bytes(
            bytes[..8]
                .try_into()
                .map_err(|_| "invalid debug sequence")?,
        );
        if id == 0 {
            return Err("invalid debug sequence");
        }
        let operation = match bytes[8] {
            1 => Operation::Append,
            2 => Operation::Execute,
            3 => Operation::Shell,
            4 => Operation::Input,
            5 => Operation::Close,
            6 => Operation::Resize,
            _ => return Err("unknown debug operation"),
        };
        Ok(Self {
            id,
            operation,
            data: bytes[9..].to_vec(),
        })
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Status {
    pub acknowledged: u64,
    pub running: bool,
    pub exit_code: Option<i32>,
}
impl Status {
    pub fn encode(self) -> Vec<u8> {
        let mut b = self.acknowledged.to_le_bytes().to_vec();
        b.push(u8::from(self.running));
        b.push(u8::from(self.exit_code.is_some()));
        b.extend_from_slice(&self.exit_code.unwrap_or(0).to_le_bytes());
        b
    }
    pub fn decode(b: &[u8]) -> Result<Self, &'static str> {
        if b.len() != 14 || b[8] > 1 || b[9] > 1 {
            return Err("invalid debug status");
        }
        Ok(Self {
            acknowledged: u64::from_le_bytes(b[..8].try_into().map_err(|_| "sequence")?),
            running: b[8] == 1,
            exit_code: (b[9] == 1).then(|| i32::from_le_bytes(b[10..14].try_into().unwrap())),
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frames_roundtrip_and_reject_truncated_or_oversized_input() {
        let m = Message {
            id: 17,
            operation: Operation::Input,
            data: b"echo hi\n".to_vec(),
        };
        let bytes = m.encode().unwrap();
        assert_eq!(Message::decode(&bytes).unwrap(), m);
        for n in 0..9 {
            assert!(Message::decode(&bytes[..n]).is_err());
        }
        let mut bad = bytes.clone();
        bad[8] = 255;
        assert!(Message::decode(&bad).is_err());
        assert!(
            Message {
                id: 1,
                operation: Operation::Append,
                data: vec![0; MAX_DATA + 1]
            }
            .encode()
            .is_err()
        );
        for exit_code in [None, Some(0), Some(137)] {
            let s = Status {
                acknowledged: 17,
                running: exit_code.is_none(),
                exit_code,
            };
            assert_eq!(Status::decode(&s.encode()).unwrap(), s);
        }
    }
}
