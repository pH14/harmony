// SPDX-License-Identifier: AGPL-3.0-or-later

pub const EVENT_ID: u32 = 0x0400_0002;
pub const MAX_LEN: u32 = 2 * 1024 * 1024;
pub const MAX_REGIONS: u32 = 16;
pub const DESCRIPTOR_LEN: usize = 24;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Descriptor {
    pub handle: u32,
    pub address: u64,
    pub len: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidDescriptor;

impl core::fmt::Display for InvalidDescriptor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("invalid observation descriptor")
    }
}

impl core::error::Error for InvalidDescriptor {}

impl Descriptor {
    pub fn decode(bytes: &[u8]) -> Result<Self, InvalidDescriptor> {
        let bytes: &[u8; DESCRIPTOR_LEN] = bytes.try_into().map_err(|_| InvalidDescriptor)?;
        let word = |at| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let descriptor = Self {
            handle: word(4),
            address: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            len: word(16),
        };
        if word(0) != 1
            || word(20) != 0
            || descriptor.handle == 0
            || descriptor.len > MAX_LEN
            || (descriptor.len == 0) != (descriptor.address == 0)
            || descriptor
                .address
                .checked_add(u64::from(descriptor.len))
                .is_none()
        {
            return Err(InvalidDescriptor);
        }
        Ok(descriptor)
    }

    pub fn encode(self) -> Result<[u8; DESCRIPTOR_LEN], InvalidDescriptor> {
        let mut bytes = [0; DESCRIPTOR_LEN];
        bytes[..4].copy_from_slice(&1_u32.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.handle.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.address.to_le_bytes());
        bytes[16..20].copy_from_slice(&self.len.to_le_bytes());
        Self::decode(&bytes)?;
        Ok(bytes)
    }

    pub fn range(self, offset: u32, len: u32) -> Result<u64, InvalidDescriptor> {
        if self.len == 0 || offset.checked_add(len).is_none_or(|end| end > self.len) {
            return Err(InvalidDescriptor);
        }
        self.address
            .checked_add(u64::from(offset))
            .ok_or(InvalidDescriptor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_and_revocation_round_trip() {
        for descriptor in [
            Descriptor {
                handle: 1,
                address: 0x4000,
                len: MAX_LEN,
            },
            Descriptor {
                handle: 1,
                address: 0,
                len: 0,
            },
        ] {
            assert_eq!(
                Descriptor::decode(&descriptor.encode().unwrap()),
                Ok(descriptor)
            );
        }
    }

    #[test]
    fn malformed_registration_never_creates_a_readable_region() {
        let descriptor = Descriptor {
            handle: 7,
            address: 0x8000,
            len: 4096,
        };
        let good = descriptor.encode().unwrap();
        for len in 0..DESCRIPTOR_LEN {
            assert!(Descriptor::decode(&good[..len]).is_err());
        }
        for at in [0, 20] {
            let mut bad = good;
            bad[at] ^= 2;
            assert!(Descriptor::decode(&bad).is_err());
        }
        for bad in [
            Descriptor {
                handle: 0,
                ..descriptor
            },
            Descriptor {
                address: u64::MAX,
                ..descriptor
            },
            Descriptor {
                address: 0,
                ..descriptor
            },
            Descriptor {
                len: 0,
                ..descriptor
            },
            Descriptor {
                len: MAX_LEN + 1,
                ..descriptor
            },
        ] {
            assert!(bad.encode().is_err());
        }
    }

    #[test]
    fn reads_are_bounded_and_revocation_prevents_access() {
        let descriptor = Descriptor {
            handle: 7,
            address: 0x8000,
            len: 4096,
        };
        assert_eq!(descriptor.range(4095, 1), Ok(0x8fff));
        assert!(descriptor.range(4095, 2).is_err());
        assert!(descriptor.range(u32::MAX, 2).is_err());
        assert!(
            Descriptor {
                address: 0,
                len: 0,
                ..descriptor
            }
            .range(0, 0)
            .is_err()
        );
    }
}
