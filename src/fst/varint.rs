//! Variable-length integer encoding for FST format
//!
//! FST uses a custom varint encoding where:
//! - Each byte uses 7 bits for data and 1 bit for continuation flag
//! - MSB=1 indicates more bytes follow
//! - MSB=0 indicates this is the last byte

/// Encode a u64 into FST varint format
#[inline]
pub fn encode_varint(mut n: u64) -> Vec<u8> {
    if n == 0 {
        return vec![0];
    }

    let mut buf = Vec::with_capacity(9);
    while n > 0x7F {
        buf.push(((n & 0x7F) as u8) | 0x80);
        n >>= 7;
    }
    buf.push(n as u8);
    buf
}

/// Encode a u64 into FST varint format, returning slice of provided buffer
/// Returns the encoded length (1-10 bytes)
#[inline]
#[allow(dead_code)]
pub fn encode_varint_buf(mut n: u64, buf: &mut [u8; 10]) -> usize {
    if n == 0 {
        buf[0] = 0;
        return 1;
    }

    let mut pos = 0;
    while n > 0x7F {
        buf[pos] = ((n & 0x7F) as u8) | 0x80;
        n >>= 7;
        pos += 1;
    }
    buf[pos] = n as u8;
    pos + 1
}

/// Encode time delta into provided buffer
#[inline]
#[allow(dead_code)]
pub fn encode_time_delta_buf(prev: u64, curr: u64, buf: &mut [u8; 10]) -> usize {
    let delta = curr.saturating_sub(prev);
    encode_varint_buf(delta, buf)
}

/// Decode a FST varint from a byte slice
/// Returns (value, bytes_consumed)
#[inline]
pub fn decode_varint(buf: &[u8]) -> Option<(u64, usize)> {
    if buf.is_empty() {
        return None;
    }

    let mut result: u64 = 0;
    let mut shift = 0;
    let mut pos = 0;

    loop {
        if pos >= buf.len() {
            return None;
        }
        let b = buf[pos];
        result |= ((b & 0x7F) as u64) << shift;
        pos += 1;
        if b & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }

    Some((result, pos))
}

/// Encode time delta (difference between consecutive timestamps)
#[inline]
#[allow(dead_code)]
pub fn encode_time_delta(prev: u64, curr: u64) -> Vec<u8> {
    let delta = curr.saturating_sub(prev);
    encode_varint(delta)
}

/// Encode time delta to existing buffer
#[inline]
#[allow(dead_code)]
pub fn encode_time_delta_to_buf(prev: u64, curr: u64, buf: &mut [u8; 10]) -> usize {
    let delta = curr.saturating_sub(prev);
    encode_varint_buf(delta, buf)
}

/// Encode a signed varint (for timezero, etc.)
#[inline]
#[allow(dead_code)]
pub fn encode_signed_varint(n: i64) -> Vec<u8> {
    // Standard protobuf zigzag encoding
    // Correctly handles all i64 values including i64::MIN
    let unsigned = ((n as u64) << 1) ^ ((n as u64) >> 63);
    encode_varint(unsigned)
}

/// Decode a signed varint (protobuf zigzag encoding)
#[inline]
#[allow(dead_code)]
pub fn decode_signed_varint(buf: &[u8]) -> Option<(i64, usize)> {
    let (unsigned, consumed) = decode_varint(buf)?;
    let n = if (unsigned & 1) == 0 {
        (unsigned >> 1) as i64
    } else {
        -((unsigned >> 1) as i64) - 1
    };
    Some((n, consumed))
}

/// Encode FST signed varint (big-endian, sign-extended raw varint).
/// Used by the DYNALIAS2 chain index table.
/// Big-endian byte order: MSB 7-bit group first, LSB group last (without MSB flag).
#[inline]
pub fn encode_fst_svarint(v: i64) -> Vec<u8> {
    let val = v as u64;
    if val == 0 {
        return vec![0x00];
    }
    let mut groups = Vec::new();
    let mut tmp = val;
    while tmp > 0 {
        groups.push((tmp & 0x7F) as u8);
        tmp >>= 7;
    }
    let mut buf = Vec::with_capacity(groups.len());
    for i in (0..groups.len()).rev() {
        if i > 0 {
            buf.push(groups[i] | 0x80);
        } else {
            buf.push(groups[i]);
        }
    }
    buf
}

/// Decode FST signed varint (NOT zigzag — sign-extended raw varint).
/// The FST DYNALIAS2 format uses this for the chain index table.
/// Uses u64 internally to avoid overflow, then casts to i64 (preserving bit pattern).
#[inline]
pub fn decode_fst_svarint(buf: &[u8]) -> Option<(i64, usize)> {
    if buf.is_empty() {
        return None;
    }

    let mut pos = 0;
    while pos < buf.len() && buf[pos] & 0x80 != 0 {
        pos += 1;
    }
    if pos >= buf.len() {
        return None;
    }
    let consumed = pos + 1;

    let mut result: u64 = 0;
    let mut shift = 0;
    loop {
        result |= ((buf[pos] & 0x7F) as u64) << shift;
        if pos == 0 {
            break;
        }
        pos -= 1;
        shift += 7;
    }

    Some((result as i64, consumed))
}

/// Decode FST varint32 (same as decode_varint but returns u32)
#[inline]
pub fn decode_fst_varint32(buf: &[u8]) -> Option<(u32, usize)> {
    let (val, consumed) = decode_varint(buf)?;
    Some((val as u32, consumed))
}

/// Read a FST signed varint from a reader
pub fn decode_fst_svarint_from_reader<R: std::io::Read>(reader: &mut R) -> Result<i64, String> {
    let mut buf = [0u8; 10];
    let mut pos = 0;
    loop {
        if pos >= buf.len() {
            return Err("Signed varint too long".to_string());
        }
        let mut byte = [0u8; 1];
        reader.read_exact(&mut byte).map_err(|e| format!("Read error: {}", e))?;
        buf[pos] = byte[0];
        pos += 1;
        if byte[0] & 0x80 == 0 {
            break;
        }
    }
    match decode_fst_svarint(&buf[..pos]) {
        Some((v, _)) => Ok(v),
        None => Err("Failed to decode signed varint".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_varint_encode_decode() {
        let test_cases = [0u64, 1, 127, 128, 255, 256, 1000, 1000000, u64::MAX];

        for n in test_cases {
            let encoded = encode_varint(n);
            let (decoded, consumed) = decode_varint(&encoded).unwrap();
            assert_eq!(n, decoded);
            assert_eq!(encoded.len(), consumed);
        }
    }

    #[test]
    fn test_time_delta() {
        let times = [0u64, 100, 101, 102, 200, 1000000];
        let mut prev = 0;
        for &t in &times {
            let delta = encode_time_delta(prev, t);
            let (decoded, _) = decode_varint(&delta).unwrap();
            assert_eq!(decoded, t - prev);
            prev = t;
        }
    }

    #[test]
    fn test_fst_svarint_roundtrip() {
        let cases = [0i64, 1, -1, 127, -127, 128, -128, 255, 256, 1000, -1000,
                     100000, 43981, 0xABCD, -43981, i64::MAX, i64::MIN];
        for &n in &cases {
            let encoded = encode_fst_svarint(n);
            let (decoded, consumed) = decode_fst_svarint(&encoded).unwrap();
            assert_eq!(n, decoded, "roundtrip failed for {}", n);
            assert_eq!(encoded.len(), consumed);
        }
    }

    #[test]
    fn test_signed_varint() {
        // Test only positive numbers for practical use in time encoding
        let test_cases = [0i64, 1, 2, 127, 128, 255, 256, 1000, 1000000, i64::MAX];

        for n in test_cases {
            let encoded = encode_signed_varint(n);
            let (decoded, consumed) = decode_signed_varint(&encoded).unwrap();
            assert_eq!(n, decoded, "encode/decode failed for {}", n);
            assert_eq!(encoded.len(), consumed);
        }
    }
}
