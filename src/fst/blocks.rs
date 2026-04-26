//! FST block serialization
//!
//! Each FST file consists of multiple blocks with the structure:
//! [block_type: u8][block_length: u64][block_data: bytes]

use super::types::{BlockType, FstHeader, ScopeType, SignalDecl, VarType};
use super::varint::{encode_signed_varint, encode_varint};

/// Block writer for serializing FST blocks
#[derive(Debug)]
#[allow(dead_code)]
pub struct BlockWriter {
    buf: Vec<u8>,
}

#[allow(dead_code)]
impl BlockWriter {
    /// Create a new block writer with specified type
    pub fn new(block_type: BlockType) -> Self {
        let mut buf = Vec::with_capacity(256);
        buf.push(block_type as u8);
        Self { buf }
    }

    /// Write a byte
    #[inline]
    pub fn write_u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    /// Write a little-endian u16
    #[inline]
    pub fn write_u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Write a little-endian u32
    #[inline]
    pub fn write_u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Write a little-endian u64
    #[inline]
    pub fn write_u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Write a signed i8
    #[inline]
    pub fn write_i8(&mut self, v: i8) {
        self.buf.push(v as u8);
    }

    /// Write a signed i64
    #[inline]
    pub fn write_i64(&mut self, v: i64) {
        let encoded = encode_signed_varint(v);
        self.buf.extend(encoded);
    }

    /// Write a varint
    #[inline]
    pub fn write_varint(&mut self, v: u64) {
        let encoded = encode_varint(v);
        self.buf.extend(encoded);
    }

    /// Write raw bytes
    #[inline]
    pub fn write_bytes(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Write a null-terminated string
    pub fn write_cstring(&mut self, s: &str) {
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
    }

    /// Write header block
    pub fn write_header(&mut self, hdr: &FstHeader, scope_count: u64, var_count: u64, max_handle: u64, vc_count: u64) {
        self.write_u64(hdr.start_time);
        self.write_u64(hdr.end_time);
        // Endian test value (pi) - required by FST format specification
        #[allow(clippy::approx_constant)]
        self.write_bytes(&3.14159265358979f64.to_le_bytes());
        self.write_u64(0); // mem_used - deprecated
        self.write_u64(scope_count);
        self.write_u64(var_count);
        self.write_u64(max_handle);
        self.write_u64(vc_count);
        self.write_i8(hdr.timescale_exp);
        // Version (128 bytes, null-padded)
        let mut version_bytes = [0u8; 128];
        let version_len = hdr.version.as_bytes().len().min(127);
        version_bytes[..version_len].copy_from_slice(&hdr.version.as_bytes()[..version_len]);
        self.write_bytes(&version_bytes);
        // Date (128 bytes, null-padded)
        let mut date_bytes = [0u8; 128];
        let date_len = hdr.date.as_bytes().len().min(127);
        date_bytes[..date_len].copy_from_slice(&hdr.date.as_bytes()[..date_len]);
        self.write_bytes(&date_bytes);
    }

    /// Write geometry block (signal metadata)
    pub fn write_geom(&mut self, signals: &[SignalDecl], max_handle: u64) {
        // Reserve space for header (will fill in later)
        let header_start = self.buf.len();
        self.write_u64(0); // section_length placeholder
        self.write_u64(0); // uncompressed_length placeholder
        self.write_u64(max_handle);

        // Write signal array
        for sig in signals {
            self.write_u32(sig.handle);
            self.write_varint(sig.name.len() as u64);
            self.write_bytes(sig.name.as_bytes());
            self.write_u8(sig.var_type as u8);
            self.write_u8(0); // direction (placeholder)
            self.write_u32(sig.width);
        }

        // Fill in actual lengths
        let data_len = self.buf.len() - header_start - 24; // subtract header
        let uncompressed = data_len as u64;
        self.write_u64_at(header_start, uncompressed + 24);
        self.write_u64_at(header_start + 8, uncompressed);
    }

    /// Write hierarchy block (scopes and variables)
    pub fn write_hier(&mut self, data: &[u8]) {
        self.write_bytes(data);
    }

    /// Write VCDATA block header
    pub fn write_vcdata_header(&mut self, time_begin: u64, time_end: u64, mem_required: u64, compressed_len: u64, max_handle: u64) {
        self.write_varint(time_begin);
        self.write_varint(time_end);
        self.write_varint(mem_required);
        self.write_varint(compressed_len);
        self.write_varint(max_handle);
    }

    /// Write a null byte
    #[inline]
    pub fn write_null(&mut self) {
        self.buf.push(0);
    }

    /// Finalize the block and return the complete block with length prefix
    pub fn finalize(self) -> Vec<u8> {
        let block_type = self.buf[0];
        let block_data_len = self.buf.len() - 1;

        let mut result = Vec::with_capacity(1 + 8 + block_data_len);
        result.push(block_type);
        result.extend_from_slice(&(block_data_len as u64).to_le_bytes());
        result.extend_from_slice(&self.buf[1..]);

        result
    }

    /// Write a u64 at a specific offset (for filling in placeholders)
    fn write_u64_at(&mut self, offset: usize, v: u64) {
        self.buf[offset..offset + 8].copy_from_slice(&v.to_le_bytes());
    }

    /// Get current position
    #[inline]
    pub fn position(&self) -> usize {
        self.buf.len()
    }
}

/// Encode a scope entry for hierarchy block
#[allow(dead_code)]
pub fn encode_scope_entry(name: &str, scope_type: ScopeType) -> Vec<u8> {
    let mut buf = Vec::with_capacity(64);
    buf.push(0x01); // HIER_SCOPE
    buf.push(scope_type as u8);
    buf.extend_from_slice(name.as_bytes());
    buf.push(0);
    buf
}

/// Encode a variable entry for hierarchy block
#[allow(dead_code)]
pub fn encode_var_entry(handle: u32, name: &str, var_type: VarType, width: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(64);
    buf.push(0x02); // HIER_VAR
    buf.push(var_type as u8);
    buf.push(0); // direction (placeholder)
    buf.extend_from_slice(name.as_bytes());
    buf.push(0);
    buf.extend_from_slice(&handle.to_le_bytes());
    buf.extend_from_slice(&width.to_le_bytes());
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_block_writer() {
        let mut bw = BlockWriter::new(BlockType::Geom);
        bw.write_u64(42);
        bw.write_bytes(b"hello");
        let block = bw.finalize();

        assert_eq!(block[0], BlockType::Geom as u8);
        let len = u64::from_le_bytes(block[1..9].try_into().unwrap());
        assert_eq!(len, 13); // 8 + 5 bytes
        // Block structure: [type(1)] [length(8)] [data(length)]
        // data = u64(42) + "hello" = [42, 0,0,0,0,0,0,0, 104,101,108,108,111]
        assert_eq!(&block[9..], &[42, 0, 0, 0, 0, 0, 0, 0, 104, 101, 108, 108, 111]);
    }

    #[test]
    fn test_encode_scope_entry() {
        let entry = encode_scope_entry("top", ScopeType::VcdModule);
        assert_eq!(entry[0], 0x01); // HIER_SCOPE
        assert_eq!(entry[1], ScopeType::VcdModule as u8);
        assert!(entry.ends_with(&[0])); // null terminator
    }
}
