// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{io, os::fd::RawFd};
fn malformed(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("session worker frame: {what}"),
    )
}

pub(crate) fn parse_control_rights(
    control: &[u8],
    reported: usize,
    mut found: impl FnMut(RawFd),
) -> io::Result<()> {
    let bound = control.len().min(reported);
    let mut at = 0usize;
    while bound.saturating_sub(at) >= size_of::<libc::cmsghdr>() {
        // SAFETY: the bounds check covers a complete cmsghdr; its fields are integers and
        // every bit pattern is valid, and read_unaligned imposes no alignment requirement.
        let header =
            unsafe { std::ptr::read_unaligned(control[at..].as_ptr().cast::<libc::cmsghdr>()) };
        // SAFETY: CMSG_LEN performs only native control-message layout arithmetic for zero bytes.
        let data_offset = unsafe { libc::CMSG_LEN(0) as usize };
        let declared = header.cmsg_len as usize;
        if declared < data_offset {
            return Err(malformed("control header length"));
        }
        let available = bound - at;
        let end = declared.min(available);
        if header.cmsg_level == libc::SOL_SOCKET
            && header.cmsg_type == libc::SCM_RIGHTS
            && end >= data_offset
        {
            let payload = &control[at + data_offset..at + end];
            for bytes in payload.chunks_exact(size_of::<RawFd>()) {
                found(RawFd::from_ne_bytes(
                    bytes.try_into().expect("complete descriptor"),
                ));
            }
            if !payload.len().is_multiple_of(size_of::<RawFd>()) {
                return Err(malformed("partial control descriptor"));
            }
        }
        if declared > available {
            return Err(malformed("truncated control payload"));
        }
        let payload = u32::try_from(declared - data_offset)
            .map_err(|_| malformed("control length overflow"))?;
        // SAFETY: CMSG_SPACE performs only native layout arithmetic; payload was bounded by
        // the received allocation and converted without truncation.
        let step = unsafe { libc::CMSG_SPACE(payload) as usize };
        if step == 0 {
            return Err(malformed("control header stride"));
        }
        at = at
            .checked_add(step)
            .ok_or_else(|| malformed("control length overflow"))?;
    }
    if reported > control.len() {
        return Err(malformed("control extent exceeds allocation"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn native_space(count: usize) -> usize {
        // SAFETY: CMSG_SPACE only computes native layout for this bounded test payload.
        unsafe { libc::CMSG_SPACE((count * size_of::<RawFd>()) as u32) as usize }
    }
    fn synthetic_rights(declared: usize, values: &[RawFd], capacity: usize) -> Vec<u8> {
        // SAFETY: cmsghdr contains integer fields; zero initializes all fields and native padding.
        let mut header: libc::cmsghdr = unsafe { std::mem::zeroed() };
        header.cmsg_len = declared as _;
        header.cmsg_level = libc::SOL_SOCKET;
        header.cmsg_type = libc::SCM_RIGHTS;
        let mut bytes = vec![0; capacity];
        let mut put = |at: usize, field: &[u8]| bytes[at..at + field.len()].copy_from_slice(field);
        put(
            std::mem::offset_of!(libc::cmsghdr, cmsg_len),
            &header.cmsg_len.to_ne_bytes(),
        );
        put(
            std::mem::offset_of!(libc::cmsghdr, cmsg_level),
            &header.cmsg_level.to_ne_bytes(),
        );
        put(
            std::mem::offset_of!(libc::cmsghdr, cmsg_type),
            &header.cmsg_type.to_ne_bytes(),
        );
        // SAFETY: CMSG_LEN only computes the native payload offset for zero bytes.
        let offset = unsafe { libc::CMSG_LEN(0) as usize };
        for (index, value) in values.iter().enumerate() {
            put(offset + index * size_of::<RawFd>(), &value.to_ne_bytes());
        }
        bytes
    }

    #[test]
    fn bounded_control_parser_ignores_declared_padding_and_partial_handles() {
        // SAFETY: CMSG_LEN only computes the native payload offset for zero bytes.
        let offset = unsafe { libc::CMSG_LEN(0) as usize };
        let word = size_of::<RawFd>();
        let complete = offset + 2 * word;
        let declared = offset + 3 * word;
        let bytes = synthetic_rights(declared, &[41, 42, 0], declared);
        let mut found = Vec::new();
        assert!(parse_control_rights(&bytes, complete, |fd| found.push(fd)).is_err());
        assert_eq!(found, [41, 42]);
        found.clear();
        assert!(parse_control_rights(&bytes[..complete], declared, |fd| found.push(fd)).is_err());
        assert_eq!(found, [41, 42]);
        found.clear();
        assert!(parse_control_rights(&bytes, offset + word + 2, |fd| found.push(fd)).is_err());
        assert_eq!(found, [41]);
        let short = synthetic_rights(offset - 1, &[], offset);
        found.clear();
        assert!(parse_control_rights(&short, short.len(), |fd| found.push(fd)).is_err());
        assert!(found.is_empty());
        parse_control_rights(&bytes, 0, |_| {
            panic!("empty returned extent has no descriptors")
        })
        .unwrap();
    }

    #[test]
    fn bounded_control_parser_walks_native_aligned_messages() {
        // SAFETY: CMSG_LEN only computes native layout for a bounded two-descriptor payload.
        let first_len = unsafe { libc::CMSG_LEN((2 * size_of::<RawFd>()) as u32) as usize };
        // SAFETY: CMSG_LEN only computes native layout for one descriptor.
        let second_len = unsafe { libc::CMSG_LEN(size_of::<RawFd>() as u32) as usize };
        let mut bytes = synthetic_rights(first_len, &[41, 42], native_space(2));
        bytes.extend_from_slice(&synthetic_rights(second_len, &[43], second_len));
        let mut found = Vec::new();
        parse_control_rights(&bytes, bytes.len(), |fd| found.push(fd)).unwrap();
        assert_eq!(found, [41, 42, 43]);
    }
}
