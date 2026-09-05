//! Low-level Bitcoin wire encoding helpers: CompactSize integers and script pushes.

/// Append a CompactSize ("varint") encoding of `n`.
pub fn write_varint(out: &mut Vec<u8>, n: u64) {
    match n {
        0..=0xfc => out.push(n as u8),
        0xfd..=0xffff => {
            out.push(0xfd);
            out.extend_from_slice(&(n as u16).to_le_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(0xfe);
            out.extend_from_slice(&(n as u32).to_le_bytes());
        }
        _ => {
            out.push(0xff);
            out.extend_from_slice(&n.to_le_bytes());
        }
    }
}

/// Number of bytes the CompactSize encoding of `n` occupies.
pub fn varint_len(n: u64) -> usize {
    match n {
        0..=0xfc => 1,
        0xfd..=0xffff => 3,
        0x1_0000..=0xffff_ffff => 5,
        _ => 9,
    }
}

/// Decode a CompactSize integer from the front of `bytes`, returning the value and the
/// number of bytes it occupied.
///
/// # Panics
/// If `bytes` is too short.
pub fn read_varint(bytes: &[u8]) -> (u64, usize) {
    match bytes[0] {
        n @ 0..=0xfc => (u64::from(n), 1),
        0xfd => (u64::from(u16::from_le_bytes([bytes[1], bytes[2]])), 3),
        0xfe => (
            u64::from(u32::from_le_bytes(bytes[1..5].try_into().unwrap())),
            5,
        ),
        _ => (u64::from_le_bytes(bytes[1..9].try_into().unwrap()), 9),
    }
}

/// Append a script data push of `data` using the minimal push opcode.
pub fn push_data(out: &mut Vec<u8>, data: &[u8]) {
    let len = data.len();
    if len < 0x4c {
        out.push(len as u8);
    } else if len <= 0xff {
        out.push(0x4c);
        out.push(len as u8);
    } else if len <= 0xffff {
        out.push(0x4d);
        out.extend_from_slice(&(len as u16).to_le_bytes());
    } else {
        out.push(0x4e);
        out.extend_from_slice(&(len as u32).to_le_bytes());
    }
    out.extend_from_slice(data);
}

/// Append the script encoding of an integer the way Bitcoin Core's `CScript << int64_t`
/// does: `OP_0`, `OP_1NEGATE`, `OP_1`..`OP_16`, or a minimal CScriptNum push.
///
/// This is what BIP34 requires at the start of the coinbase scriptSig.
pub fn push_script_num(out: &mut Vec<u8>, n: i64) {
    match n {
        0 => out.push(0x00),
        -1 => out.push(0x4f),
        1..=16 => out.push(0x50 + n as u8),
        _ => {
            let bytes = script_num_bytes(n);
            push_data(out, &bytes);
        }
    }
}

/// Minimal little-endian sign-magnitude encoding used by CScriptNum.
pub fn script_num_bytes(n: i64) -> Vec<u8> {
    if n == 0 {
        return Vec::new();
    }
    let negative = n < 0;
    let mut abs = n.unsigned_abs();
    let mut out = Vec::with_capacity(9);
    while abs > 0 {
        out.push((abs & 0xff) as u8);
        abs >>= 8;
    }
    // If the top bit of the last byte is set, add a byte so the sign bit is free.
    if out.last().is_some_and(|b| b & 0x80 != 0) {
        out.push(if negative { 0x80 } else { 0x00 });
    } else if negative {
        let last = out.len() - 1;
        out[last] |= 0x80;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varint(n: u64) -> Vec<u8> {
        let mut v = Vec::new();
        write_varint(&mut v, n);
        v
    }

    #[test]
    fn varint_round_trips() {
        for n in [
            0u64,
            1,
            0xfc,
            0xfd,
            0xffff,
            0x1_0000,
            0xffff_ffff,
            0x1_0000_0000,
        ] {
            let bytes = varint(n);
            assert_eq!(read_varint(&bytes), (n, bytes.len()), "{n}");
            assert_eq!(varint_len(n), bytes.len());
        }
    }

    #[test]
    fn varint_boundaries() {
        assert_eq!(varint(0), [0x00]);
        assert_eq!(varint(0xfc), [0xfc]);
        assert_eq!(varint(0xfd), [0xfd, 0xfd, 0x00]);
        assert_eq!(varint(0xffff), [0xfd, 0xff, 0xff]);
        assert_eq!(varint(0x1_0000), [0xfe, 0x00, 0x00, 0x01, 0x00]);
        assert_eq!(varint(0x1_0000_0000), [0xff, 0, 0, 0, 0, 1, 0, 0, 0]);
    }

    #[test]
    fn script_num_matches_core() {
        // Heights seen in real coinbases: 1 -> OP_1, 17 -> 0x0111, 500000 -> 0x0320a107.
        let mut s = Vec::new();
        push_script_num(&mut s, 1);
        assert_eq!(s, [0x51]);
        s.clear();
        push_script_num(&mut s, 17);
        assert_eq!(s, [0x01, 0x11]);
        s.clear();
        push_script_num(&mut s, 500_000);
        assert_eq!(s, [0x03, 0x20, 0xa1, 0x07]);
        s.clear();
        push_script_num(&mut s, 128);
        assert_eq!(s, [0x02, 0x80, 0x00]);
        s.clear();
        push_script_num(&mut s, 2_500_000);
        assert_eq!(s, [0x03, 0xa0, 0x25, 0x26]);
    }

    #[test]
    fn push_data_opcodes() {
        let mut s = Vec::new();
        push_data(&mut s, &[0xab; 3]);
        assert_eq!(s, [0x03, 0xab, 0xab, 0xab]);
        s.clear();
        push_data(&mut s, &[0; 80]);
        assert_eq!(&s[..2], &[0x4c, 80]);
        assert_eq!(s.len(), 82);
    }
}
