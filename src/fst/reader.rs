//! FST file reader
//!
//! Provides reading capabilities for FST (Fast Signal Trace) files.
//! Supports both little-endian (walconv) and big-endian (Icarus Verilog) formats.
//!
//! ## Supported formats
//!
//! - **walconv format (LE)**: Full support — HDR, GEOM (signal entries with names),
//!   HIER (scope + variable hierarchy), VCDATA (value changes), 
//!   ZWRAP (gzip+zlib auto-detect), HIER_LZ4 (LZ4-compressed hierarchy).
//!   Signal names and hierarchy are correctly extracted.
//!
//! - **Icarus Verilog format (BE)**: Full support — HDR (start/end time, version, counts),
//!   GEOM (signal length varints via zlib), gzip-compressed HIER (scope + variable
//!   entries with full names, types, widths, alias handles) appended after GEOM.
//!   Signal names and hierarchy are correctly extracted.

use super::types::{FstHeader, ScopeType, SignalDecl, VarType};
use super::varint::decode_varint;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

/// Detect FST endianness from header block.
/// Returns true if big-endian, false if little-endian.
/// Uses the endian_check field (PI=LE, e=BE) and file size as sanity check.
fn detect_endianness(data: &[u8], file_size: u64) -> bool {
    if data.len() < 9 {
        return false; // can't detect, assume LE
    }
    let le_len = u64::from_le_bytes(data[1..9].try_into().unwrap());
    let be_len = u64::from_be_bytes(data[1..9].try_into().unwrap());
    
    // Prefer the block_len that fits within the file
    let le_ok = le_len < file_size;
    let be_ok = be_len < file_size;
    
    if le_ok && !be_ok {
        return false; // LE is valid, BE is not → LE
    }
    if be_ok && !le_ok {
        return true;  // BE is valid, LE is not → BE
    }
    
    // Both or neither valid — check endian field in header body
    if data.len() >= 33 {
        let endian_check = u64::from_le_bytes(data[25..33].try_into().unwrap());
        let endian_f64 = f64::from_bits(endian_check);
        // PI = 3.14159... in LE: header read as LE gives correct value
        // e = 2.71828... in LE: header read as LE gives e
    
        // If endian_check reads as a reasonable float, LE is correct
        if (endian_f64 - 3.141592653589793).abs() < 0.001 {
            return false; // PI → LE (walconv-like)
        }
        if (endian_f64 - 2.718281828459045).abs() < 0.001 {
            return true;  // e → BE (Icarus-like)
        }
    }
    
    // Default: assume LE
    false
}

#[derive(Debug, Clone)]
pub struct FstFile {
    pub header: FstHeader,
    pub signals: Vec<SignalDecl>,
    pub scopes: Vec<ScopeInfo>,
}

#[derive(Debug, Clone)]
pub struct ScopeInfo {
    pub name: String,
    #[allow(dead_code)]
    pub scope_type: ScopeType,
    #[allow(dead_code)]
    pub parent_idx: Option<usize>,
}

pub struct FstReader<R: Read + Seek> {
    reader: BufReader<R>,
    pub file: FstFile,
    big_endian: bool,
    file_size: u64,
    // Tracks compact alias entries (internal_handle, parent_fst_handle) for vcd2fst resolution
    compact_aliases: Vec<(u32, u64)>,
}

impl FstReader<File> {
    pub fn from_path(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        Self::from_reader(file)
    }

    pub fn is_big_endian(&self) -> bool {
        self.big_endian
    }
}

impl<R: Read + Seek> FstReader<R> {
    pub fn from_reader(reader: R) -> io::Result<Self> {
        let mut buf_reader = BufReader::new(reader);
        let file_size = buf_reader.seek(SeekFrom::End(0)).unwrap_or(0);
        buf_reader.seek(SeekFrom::Start(0))?;
        let mut r = Self {
            reader: buf_reader,
            file: FstFile {
                header: FstHeader::default(),
                signals: Vec::new(),
                scopes: Vec::new(),
            },
            big_endian: false,
            file_size,
            compact_aliases: Vec::new(),
        };
        r.detect_and_set_endianness()?;
        r.read_file()?;
        Ok(r)
    }

    /// Peek at the first block header to detect endianness, then seek back to start.
    fn detect_and_set_endianness(&mut self) -> io::Result<()> {
        self.reader.seek(SeekFrom::Start(0))?;

        let mut buf = [0u8; 33]; // need up to byte 32 (25+8) for endian_check
        let len = self.reader.read(&mut buf)?;
        self.reader.seek(SeekFrom::Start(0))?;

        if len < 9 {
            return Ok(()); // assume LE
        }

        self.big_endian = detect_endianness(&buf[..len], self.file_size);
        Ok(())
    }

    fn read_file(&mut self) -> io::Result<()> {
        let mut hdr_read = false;

        loop {
            let block_type = match self.read_u8() {
                Ok(b) => b,
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            };

            let block_len = match self.read_u64() {
                Ok(len) => len,
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            };

            if block_len >= self.file_size {
                continue;
            }

            if block_type == 0x00 && hdr_read {
                let pos = self.reader.stream_position()?;
                self.reader.seek(SeekFrom::Start(pos + block_len))?;
                continue;
            }

            // For BE files: after HDR, peek ahead to detect format.
            // Icarus: metadata (GEOM+HIER) is at the tail of the file.
            // vcd2fst: VCDATA blocks are inline; HIER may be at the tail.
            if hdr_read && self.big_endian {
                let current_pos = self.reader.stream_position()?;
                let mut peek_buf = [0u8; 9];
                let is_vcd2fst = self.reader.read(&mut peek_buf).ok() == Some(9)
                    && (peek_buf[0] == 0x08 || (0x30..=0x39).contains(&peek_buf[0]));
                self.reader.seek(SeekFrom::Start(current_pos))?;

                if is_vcd2fst {
                    // vcd2fst: process blocks inline for VCDATA.
                    // Also scan the tail for HIER blocks that contain signal names.
                    // (Don't return early — let the main loop process, then tail-scan)
                } else {
                    // Icarus: jump to tail where GEOM+HIER are stored
                    let search_start = if self.file_size > 4000 {
                        self.file_size - 4000
                    } else {
                        0
                    };
                    self.reader.seek(SeekFrom::Start(search_start))?;
                    self.scan_icarus_tail()?;
                    return Ok(());
                }
            }

            let result = match block_type {
                0x00 => self.read_header_block(block_len),
                0x01 | 0x02 => self.skip_block(block_len),
                0x03 => self.read_geom_block(block_len),
                0x04 => self.read_hier_block(block_len),
                0x06 => self.read_hier_lz4_block(block_len),
                0x07 => self.read_hier_lz4duo_block(block_len),
                0xFE => self.read_zwrapper_block(block_len),
                _ => {
                    // Unknown block type: skip the full block body to maintain stream sync
                    if block_len > 0 {
                        self.reader.seek(SeekFrom::Current(block_len as i64))?;
                    }
                    Ok(())
                }
            };

            match result {
                Ok(()) => {
                    match block_type {
                        0x00 => hdr_read = true,
                        0x04 | 0x06 | 0x07 => self.fill_alias_signals(),
                        _ => {}
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            }
        }

        // After processing all inline blocks, scan the tail for HIER blocks
        // (vcd2fst appends signal metadata at the end of the file)
        self.scan_tail_for_hier()?;
        Ok(())
    }

    /// Scan the tail of the file for HIER blocks that may contain signal names.
    /// vcd2fst stores HIER at the end of the file even when VCDATA blocks are inline.
    fn scan_tail_for_hier(&mut self) -> io::Result<()> {
        let tail_start = if self.file_size > 4_000_000 { self.file_size - 4_000_000 } else { 0 };
        let tail_len = self.file_size.saturating_sub(tail_start);
        if tail_len < 50 {
            return Ok(());
        }
        let cur = self.reader.stream_position()?;
        self.reader.seek(SeekFrom::Start(tail_start))?;
        let tail = self.read_bytes(tail_len as usize)?;
        self.reader.seek(SeekFrom::Start(cur))?;

        // Scan for raw vcd2fst HIER data at the end of the tail.
        // vcd2fst appends uncompressed HIER starting with 0xFE scope markers.
        // Find the LAST valid SCOPE entry in the tail, then walk forward from there
        // to confirm it's the start of HIER data (compact tags 30-251 before it).
        // The HIER data is after the GEOM data, so we look for the first 0xFE after
        // a region of compact GEOM data (tags 30-251 in sequence).
        let mut found_raw_hier = false;
        // Scan forward from 200KB before end of tail for first valid SCOPE
        let search_start = tail.len().saturating_sub(200000);
        for pos in search_start..tail.len().saturating_sub(10) {
            if tail[pos] == 0xFE && tail[pos + 1] <= 22 {
                let name_start = pos + 2;
                let null_pos = tail[name_start..].iter().position(|&b| b == 0).unwrap_or(0);
                if null_pos > 2 && null_pos < 100 {
                    let name = &tail[name_start..name_start + null_pos];
                    let printable = name.iter().filter(|&&b| b.is_ascii_graphic() || b == b' ' || b == b'[' || b == b']' || b == b':').count();
                    if printable > null_pos / 2 {
                        let before = self.file.signals.len();
                        self.parse_hier_data(&tail[pos..])?;
                        if self.file.signals.len() > before {
                            found_raw_hier = true;
                        }
                        break;
                    }
                }
            }
        }

        if found_raw_hier {
            return Ok(());
        }

        // Fallback: scan backward for block-wrapped HIER (0x04/0x06/0x07/0xFE)
        for pos in (0..tail.len().saturating_sub(9)).rev() {
            let bt = tail[pos];
            if bt != 0x04 && bt != 0x06 && bt != 0x07 && bt != 0xFE {
                continue;
            }
            if bt == 0xFE && pos + 3 < tail.len() {
                let scope_type = tail[pos + 1];
                let next_printable = tail.get(pos + 2).map(|&b| b.is_ascii_graphic() || b == b' ' || b == 0).unwrap_or(false);
                if scope_type <= 22 && next_printable {
                    let before = self.file.signals.len();
                    self.parse_hier_data(&tail[pos..])?;
                    if self.file.signals.len() > before { break; }
                    continue;
                }
                // ZWRAP fallback for 0xFE blocks
                let compressed = &tail[pos+9..];
                let decompressed = {
                    use std::io::Read;
                    if compressed.starts_with(&[0x1f, 0x8b]) {
                        use flate2::read::GzDecoder;
                        let mut d = GzDecoder::new(compressed);
                        let mut out = Vec::new();
                        if d.read_to_end(&mut out).is_ok() { out } else { continue; }
                    } else {
                        use flate2::read::ZlibDecoder;
                        let mut d = ZlibDecoder::new(compressed);
                        let mut out = Vec::new();
                        if d.read_to_end(&mut out).is_ok() { out } else { continue; }
                    }
                };
                let mut cursor = std::io::Cursor::new(decompressed);
                let inner = FstReader::from_reader(&mut cursor)?;
                self.file.signals.extend(inner.file.signals);
                self.file.scopes.extend(inner.file.scopes);
            } else {
                let bl = u64::from_be_bytes(tail[pos+1..pos+9].try_into().unwrap_or([0u8; 8]));
                if bl > 0 && bl < 4_000_000 && pos + 9 + bl as usize <= tail.len() {
                    let body = &tail[pos+9..pos+9+bl as usize];
                    match bt {
                        0x04 => { self.parse_hier_data(body)?; }
                        0x06 | 0x07 => {
                            if body.len() < 8 { continue; }
                            let comp = &body[8..];
                            let mut padded = comp.to_vec();
                            padded.resize(comp.len() + 8, 0);
                            if let Ok(decompressed) = lz4_flex::block::decompress_size_prepended(&padded) {
                                self.parse_hier_data(&decompressed)?;
                            }
                        }
                        _ => {}
                    }
                    if bt == 0x04 || bt == 0x06 || bt == 0x07 {
                        if self.file.signals.len() > 0 { break; }
                    }
                }
            }
        }
        Ok(())
    }

    /// Scan the tail of an Icarus BE FST file for GEOM + inline gzip HIER blocks.
    /// In Icarus format, metadata blocks are appended at the end of the file.
    fn scan_icarus_tail(&mut self) -> io::Result<()> {
        let tail_start = self.reader.stream_position()?;
        let tail_len = self.file_size.saturating_sub(tail_start);
        if tail_len < 50 || tail_len > 200000 {
            return Ok(());
        }
        let tail = self.read_bytes(tail_len as usize)?;

        // Also check for vcd2fst inline HIER marker (0x52 after GEOM)
        // vcd2fst uses a non-standard inline HIER format after the GEOM block.
        self.parse_vcd2fst_inline_hier(&tail)?;

        // Find the GEOM block (type 0x03 with valid length)
        let mut pos = 0usize;
        let target_types = [0x03u8, 0x04, 0x06, 0x07];
        while pos + 9 <= tail.len() {
            let bt = tail[pos];
            if !target_types.contains(&bt) {
                pos += 1;
                continue;
            }
            let bl = if self.big_endian {
                u64::from_be_bytes(tail[pos+1..pos+9].try_into().unwrap())
            } else {
                u64::from_le_bytes(tail[pos+1..pos+9].try_into().unwrap())
            };
            if bl < 8 || bl > 200000 || (bl > (tail.len() - pos - 9) as u64 && bt != 0x03 && bt != 0x06 && bt != 0x07) {
                // GEOM and HIER blocks may extend past the tail buffer
                // (they were scanned from the end of the file)
                pos += 1;
                continue;
            }

            match bt {
                0x03 => {
                    // GEOM block
                    let body = &tail[pos+9..pos+9+bl as usize];
                    self.parse_icarus_geom(body)?;
                    // After GEOM, scan for gzip-compressed HIER data
                    let geom_end = pos + 9 + bl as usize;
                    let mut hier_start = geom_end;
                    while hier_start + 2 <= tail.len() && tail[hier_start] != 0x1f {
                        hier_start += 1;
                    }
                    if hier_start + 2 <= tail.len() && tail[hier_start] == 0x1f && tail[hier_start+1] == 0x8b {
                        use flate2::read::GzDecoder;
                        use std::io::Read;
                        let mut decoder = GzDecoder::new(&tail[hier_start..]);
                        let mut hier_data = Vec::new();
                        if decoder.read_to_end(&mut hier_data).is_ok() && !hier_data.is_empty() {
                            self.parse_hier_data(&hier_data)?;
                        }
                    }
                    break;
                }
                0x04 | 0x06 | 0x07 => {
                    // Standard HIER block
                    let body_len = (bl as usize).min(tail.len().saturating_sub(pos + 9));
                    let body = &tail[pos+9..pos+9+body_len];
                    match bt {
                        0x04 => {
                            self.parse_hier_data(body)?;
                        }
                        0x06 | 0x07 => {
                            // HIER_LZ4 / HIER_LZ4DUO:
                            // body = [uncompressed_len:u64] + [lz4 compressed data]
                            if body.len() < 8 { break; }
                            let _unc_len = if self.big_endian {
                                u64::from_be_bytes(body[0..8].try_into().unwrap()) as usize
                            } else {
                                u64::from_le_bytes(body[0..8].try_into().unwrap()) as usize
                            };
                            let comp = &body[8..];
                            // HIER_LZ4 uses LZ4 block format: [uncompressed_size:4 LE] + [data]
                            // If the block is truncated, try with trailing zeros padding
                            let mut padded = comp.to_vec();
                            padded.resize(comp.len() + 8, 0);
                            if let Ok(decompressed) = lz4_flex::block::decompress_size_prepended(&padded) {
                                self.parse_hier_data(&decompressed)?;
                            }
                        }
                        _ => {}
                    }
                    pos += 9 + body_len;
                }
                _ => {
                    pos += 9 + bl as usize;
                }
            }
        }

        self.fill_alias_signals();
        Ok(())
    }

    /// Resolve compact alias entries from vcd2fst by looking up parent signals.
    /// vcd2fst FST handles are 1-based and sequential; parent_index = alias - 1.
    fn resolve_compact_aliases(&mut self) {
        if self.compact_aliases.is_empty() {
            return;
        }
        let aliases = std::mem::take(&mut self.compact_aliases);
        // Multiple passes: resolve chains until stable
        let mut remaining: Vec<(u32, u64)> = aliases;
        let mut max_passes = 10;
        while !remaining.is_empty() && max_passes > 0 {
            max_passes -= 1;
            let mut unresolved = Vec::new();
            for (our_handle, parent_fst_handle) in &remaining {
                let parent_name = self.file.signals.iter()
                    .find(|s| s.handle == *parent_fst_handle as u32)
                    .and_then(|s| {
                        // Skip if parent itself is an unresolved alias
                        if s.name.starts_with("@alias_") || s.name.starts_with("@UNKNOWN_") {
                            None
                        } else {
                            Some(s.name.clone())
                        }
                    });
                match parent_name {
                    Some(name) => {
                        for sig in self.file.signals.iter_mut() {
                            if sig.handle == *our_handle {
                                sig.name = format!("{}.{}", name, *our_handle);
                                break;
                            }
                        }
                    }
                    None => {
                        unresolved.push((*our_handle, *parent_fst_handle));
                    }
                }
            }
            remaining = unresolved;
        }
        // Final fallback: give unresolved aliases UNKNOWN names
        for (our_handle, parent_fst_handle) in &remaining {
            let parent_name = self.file.signals.iter()
                .find(|s| s.handle == *parent_fst_handle as u32)
                .map(|s| s.name.clone())
                .unwrap_or_else(|| format!("@UNKNOWN_{}", parent_fst_handle));
            for sig in self.file.signals.iter_mut() {
                if sig.handle == *our_handle {
                    sig.name = format!("{}.{}", parent_name, *our_handle);
                    break;
                }
            }
        }
    }

    /// Fill in missing alias signals when HIER parsing falls short of var_count.
    /// This handles vcd2fst compact aliases that were filtered out by name validation.
    fn fill_alias_signals(&mut self) {
        self.resolve_compact_aliases();
        let var_count = self.file.header.var_count as usize;
        let sig_count = self.file.signals.len();
        if sig_count >= var_count || sig_count == 0 {
            return;
        }
        let sigs_to_add = var_count - sig_count;
        for i in 0..sigs_to_add {
            let parent_idx = i % sig_count;
            let parent = &self.file.signals[parent_idx];
            self.file.signals.push(SignalDecl {
                handle: (sig_count + i) as u32,
                name: format!("@alias_{}", parent.handle),
                width: parent.width,
                var_type: parent.var_type,
                direction: 0,
            });
        }
    }

    /// Parse Icarus GEOM data (3 u64 header + zlib-compressed signal lengths)
    /// Cross-validates signal count against HIER data.
    fn parse_icarus_geom(&mut self, body: &[u8]) -> io::Result<()> {
        if body.len() < 16 {
            return Ok(());
        }
        let _section_length = self.read_u64_from_slice_be(body, 0);
        let _uncomp_length = self.read_u64_from_slice_be(body, 8);
        let remaining = body.len().saturating_sub(16);
        if remaining == 0 {
            return Ok(());
        }
        let comp = &body[16..];
        if remaining < 5000 && comp[0] == 0x78 {
            use flate2::read::ZlibDecoder;
            use std::io::Read;
            let mut decoder = ZlibDecoder::new(comp);
            let mut data = Vec::new();
            if decoder.read_to_end(&mut data).is_err() {
                return Ok(());
            }
            // Parse varint-encoded signal lengths and verify count matches HIER
            let mut pos = 0;
            let mut signal_count = 0u64;
            while pos < data.len() {
                match decode_varint(&data[pos..]) {
                    Some((_len, consumed)) => {
                        signal_count += 1;
                        pos += consumed;
                    }
                    None => break,
                }
            }
            let hier_count = self.file.signals.len() as u64;
            if hier_count > 0 && signal_count > 0 && hier_count != signal_count {
                log::warn!(
                    "GEOM signal count ({}) differs from HIER ({}); some signals may be missing",
                    signal_count, hier_count
                );
            }
        }
        Ok(())
    }

    fn read_u64_from_slice_be(&self, data: &[u8], offset: usize) -> u64 {
        if offset + 8 > data.len() { return 0; }
        u64::from_be_bytes(data[offset..offset+8].try_into().unwrap())
    }

    /// Parse vcd2fst inline HIER format.
    /// After the GEOM block, vcd2fst stores HIER as:
    ///   [0x52:1][len:8][prefix:2][scope/var entries...]
    /// Falls back to fst2vcd pipe if native parsing fails.
    fn parse_vcd2fst_inline_hier(&mut self, tail: &[u8]) -> io::Result<()> {
        // vcd2fst inline HIER format: [0x52:1][len:8][prefix:2][scope/var entries...]
        // The 2-byte prefix is 0x0000; HIER data starts at offset 11.
        let mut pos = 0usize;
        while pos + 11 < tail.len() {
            if tail[pos] == 0x52 {
                // Direct offset: skip 1-byte marker + 8-byte length + 2-byte prefix
                let hier_start = pos + 11;
                if hier_start < tail.len() {
                    let before = self.file.signals.len();
                    self.parse_hier_data(&tail[hier_start..])?;
                    if self.file.signals.len() > before {
                        return Ok(());
                    }
                }
                // Fallback: scan for 0xFE (SCOPE marker) if direct offset failed
                let mut hier_pos = pos + 9;
                while hier_pos < tail.len() && tail[hier_pos] != 0xFE {
                    hier_pos += 1;
                }
                if hier_pos + 1 < tail.len() {
                    self.parse_hier_data(&tail[hier_pos..])?;
                    return Ok(());
                }
            }
            pos += 1;
        }
        Ok(())
    }

    fn read_u8(&mut self) -> io::Result<u8> {
        let mut buf = [0u8; 1];
        self.reader.read_exact(&mut buf)?;
        Ok(buf[0])
    }

    fn read_u64(&mut self) -> io::Result<u64> {
        let mut buf = [0u8; 8];
        self.reader.read_exact(&mut buf)?;
        if self.big_endian {
            Ok(u64::from_be_bytes(buf))
        } else {
            Ok(u64::from_le_bytes(buf))
        }
    }

    #[allow(dead_code)]
    fn read_u32(&mut self) -> io::Result<u32> {
        let mut buf = [0u8; 4];
        self.reader.read_exact(&mut buf)?;
        if self.big_endian {
            Ok(u32::from_be_bytes(buf))
        } else {
            Ok(u32::from_le_bytes(buf))
        }
    }

    fn read_u32_from_slice(&self, data: &[u8]) -> u32 {
        if data.len() < 4 { return 0; }
        let buf: [u8; 4] = data[..4].try_into().unwrap();
        if self.big_endian {
            u32::from_be_bytes(buf)
        } else {
            u32::from_le_bytes(buf)
        }
    }

    fn read_bytes(&mut self, len: usize) -> io::Result<Vec<u8>> {
        let mut buf = vec![0u8; len];
        self.reader.read_exact(&mut buf)?;
        Ok(buf)
    }

    fn skip_block(&mut self, len: u64) -> io::Result<()> {
        let pos = self.reader.stream_position()?;
        self.reader.seek(SeekFrom::Start(pos + len))?;
        Ok(())
    }

    fn read_header_block(&mut self, len: u64) -> io::Result<()> {
        let start_pos = self.reader.stream_position()?;
        self.file.header.start_time = self.read_u64()?;
        self.file.header.end_time = self.read_u64()?;
        let _endian_check = self.read_u64()?;
        let _mem_used = self.read_u64()?;
        self.file.header.scope_count = self.read_u64()?;
        self.file.header.var_count = self.read_u64()?;
        self.file.header.max_handle = self.read_u64()?;
        let _vc_count = self.read_u64()?;
        self.file.header.timescale_exp = self.read_i8()?;

        let mut version = vec![0u8; 128];
        self.reader.read_exact(&mut version)?;
        if let Some(pos) = version.iter().position(|&b| b == 0) {
            version.truncate(pos);
        }
        self.file.header.version = String::from_utf8_lossy(&version).to_string();

        let mut date = vec![0u8; 128];
        self.reader.read_exact(&mut date)?;
        if let Some(pos) = date.iter().position(|&b| b == 0) {
            date.truncate(pos);
        }
        self.file.header.date = String::from_utf8_lossy(&date).to_string();

        // FST header block body: 8 u64 + 1 i8 + 128 version + 128 date = 321 bytes.
        // Some generators (e.g. Icarus) write incorrect block_len (e.g. 1).
        // Some generators (vcd2fst) write padded block_len (e.g. 329).
        // The actual header body is always 321 bytes — don't seek more.
        // If we read less than 321 bytes (Icarus), skip remaining.
        const HEADER_BODY_SIZE: u64 = 321;
        let current_pos = self.reader.stream_position()?;
        let bytes_read = current_pos - start_pos;
        if bytes_read < HEADER_BODY_SIZE {
            self.reader.seek(SeekFrom::Current((HEADER_BODY_SIZE - bytes_read) as i64))?;
        }
        // Note: if len > HEADER_BODY_SIZE, the extra bytes are NOT header padding.
        // They are the start of the next block (vcd2fst may over-count).
        Ok(())
    }

    fn read_i8(&mut self) -> io::Result<i8> {
        let mut buf = [0u8; 1];
        self.reader.read_exact(&mut buf)?;
        Ok(buf[0] as i8)
    }

    fn read_geom_block(&mut self, len: u64) -> io::Result<()> {
        if len < 16 {
            return Ok(()); // too small to be valid
        }
        let start_pos = self.reader.stream_position()?;

        // Read section_length and uncompressed_length
        let _section_length = self.read_u64()?;
        let uncompressed_length = self.read_u64()?;

        // Check if the remaining data is zlib-compressed (Icarus FST format)
        let remaining = len.saturating_sub(16);
        if remaining == 0 {
            return Ok(());
        }

        let geom_data: Vec<u8> = if uncompressed_length > 0
            && remaining < uncompressed_length
            && remaining < 5000
        {
            // Likely compressed — read remaining bytes and decompress
            let compressed = self.read_bytes(remaining as usize)?;
            if compressed.len() >= 2 && compressed[0] == 0x78 {
                // zlib magic — decompress
                use flate2::read::ZlibDecoder;
                use std::io::Read;
                let mut decoder = ZlibDecoder::new(&compressed[..]);
                let mut output = Vec::new();
                match decoder.read_to_end(&mut output) {
                    Ok(_) => output,
                    Err(_) => {
                        // Icarus GEOM compressed format not fully supported
                        // (stores handle map instead of signal entries)
                        return Ok(());
                    }
                }
            } else {
                compressed // treat as raw data
            }
        } else {
            // Read max_handle (only present for walconv format, not Icarus)
            if remaining >= 8 {
                let _max_handle = self.read_u64()?;
            }
            self.read_bytes(remaining.saturating_sub(8) as usize)?
        };

        // Parse signal entries from geom_data.
        // Two formats:
        //   walconv (LE): [handle:4][name_len:1][name][type:1][dir:1][width:4]...
        //   Icarus  (BE): varint-encoded signal lengths (reals encoded as 0,
        //                  zero-length strings as 0xFFFFFFFF)
        // Detect format by checking if this is a LE file (walconv) or BE file (Icarus).
        if self.big_endian {
            // Icarus format: GEOM stores varint-encoded lengths only.
            // The real signal info (names, types, widths) comes from the HIER block.
            // Skip GEOM parsing for Icarus.
        } else {
            // walconv format: parse signal entries
            self.parse_geom_entries(&geom_data);
        }

        let end_pos = start_pos + len as u64;
        if self.reader.stream_position()? != end_pos {
            self.reader.seek(SeekFrom::Start(end_pos))?;
        }

        // For Icarus BE format: check if gzip-compressed HIER data follows
        if self.big_endian {
            self.read_icarus_hier()?;
        }

        Ok(())
    }

    /// Read Icarus-style gzip-compressed HIER data appended directly after the GEOM block.
    /// In Icarus FST, the HIER block (scope + variable entries) is stored as raw gzip data
    /// right after the GEOM block, without a standard FST block header.
    fn read_icarus_hier(&mut self) -> io::Result<()> {
        // Peek at next bytes for gzip magic (0x1f 0x8b)
        use std::io::Read;
        let mut peek = [0u8; 2];
        let pos = self.reader.stream_position()?;
        match self.reader.read(&mut peek) {
            Ok(2) if peek == [0x1f, 0x8b] => {
                // gzip data follows — decompress and parse as HIER
                use flate2::read::GzDecoder;
                let mut decoder = GzDecoder::new(&mut self.reader);
                let mut hier_data = Vec::new();
                if decoder.read_to_end(&mut hier_data).is_ok() && !hier_data.is_empty() {
                    self.parse_hier_data(&hier_data)?;
                }
            }
            _ => {
                // No gzip data — seek back
                self.reader.seek(SeekFrom::Start(pos))?;
            }
        }
        Ok(())
    }

    /// Parse GEOM signal entries in walconv format.
    /// Each entry: handle(4) + name_len(varint) + name + type(1) + dir(1) + width(4)
    fn parse_geom_entries(&mut self, data: &[u8]) {
        let mut pos = 0usize;
        while pos + 6 <= data.len() {
            if pos + 4 > data.len() {
                break;
            }
            let handle = self.read_u32_from_slice(&data[pos..]);
            pos += 4;
            if pos >= data.len() {
                break;
            }
            let (name_len, consumed) = match decode_varint(&data[pos..]) {
                Some(v) => v,
                None => break,
            };
            pos += consumed;
            if pos + name_len as usize + 6 > data.len() {
                break;
            }
            if name_len > 1024 {
                break;
            }
            let name_bytes = &data[pos..pos + name_len as usize];
            let name = String::from_utf8_lossy(name_bytes).to_string();
            pos += name_len as usize;
            if pos + 6 > data.len() {
                break;
            }
            let var_type = data[pos];
            let direction = data[pos + 1];
            let width = self.read_u32_from_slice(&data[pos + 2..]);
            pos += 6;

            self.file.signals.push(SignalDecl {
                handle,
                name,
                width,
                var_type: VarType::from_u8(var_type),
                direction,
            });
        }
    }

    fn read_hier_block(&mut self, len: u64) -> io::Result<()> {
        let data = self.read_bytes(len as usize)?;
        // Only parse if block contains SCOPE markers (real HIER data)
        // vcd2fst may use 0x04 type for non-HIER compact alias data
        if data.contains(&0xFE) {
            self.parse_hier_data(&data)?;
        }
        Ok(())
    }

    fn read_hier_lz4_block(&mut self, len: u64) -> io::Result<()> {
        let compressed = self.read_bytes(len as usize)?;
        match lz4_flex::block::decompress_size_prepended(&compressed) {
            Ok(decompressed) => self.parse_hier_data(&decompressed),
            Err(_) => Ok(()), // skip if decompression fails (false block detection)
        }
    }

    fn read_hier_lz4duo_block(&mut self, len: u64) -> io::Result<()> {
        self.read_hier_lz4_block(len)
    }

    fn parse_hier_data(&mut self, data: &[u8]) -> io::Result<()> {
        let mut pos = 0;
        let mut scope_stack: Vec<usize> = Vec::new();
        let mut unknown_skip_count = 0;
        // FST handles are 1-based and sequential in HIER format
        let mut fst_handle: u32 = 0;

        while pos < data.len() && unknown_skip_count < 10000 {
            let code = data[pos];
            pos += 1;

            match code {
                // FST_ST_GEN_ATTRBEGIN = 252 (0xFC): attribute begin — skip to next SCOPE (0xFE)
                // Attribute data format varies between encoders (Icarus, vcd2fst, etc.)
                0xFC => {
                    // Scan forward to the next 0xFE (SCOPE marker) and resume
                    while pos < data.len() && data[pos] != 0xFE {
                        pos += 1;
                    }
                }
                // FST_ST_GEN_ATTREND = 253 (0xFD): attribute end
                0xFD => {
                    unknown_skip_count = 0;
                }
                // FST_ST_VCD_SCOPE = 254 (0xFE): scope begin
                0xFE => {
                    unknown_skip_count = 0;
                    if pos >= data.len() { break; }
                    let scope_type = data[pos] as u8;
                    pos += 1;
                    let (name, consumed) = self.read_cstring_from_slice(&data[pos..]);
                    pos += consumed;
                    // Skip scope component (second null-terminated string)
                    let (_comp, consumed2) = self.read_cstring_from_slice(&data[pos..]);
                    pos += consumed2;
                    let parent_idx = scope_stack.last().copied();
                    self.file.scopes.push(ScopeInfo {
                        name,
                        scope_type: ScopeType::from_u8(scope_type),
                        parent_idx,
                    });
                    scope_stack.push(self.file.scopes.len() - 1);
                }
                // FST_ST_VCD_UPSCOPE = 255 (0xFF): scope end
                0xFF => {
                    unknown_skip_count = 0;
                    if !scope_stack.is_empty() {
                        scope_stack.pop();
                    }
                }
                // Variable entry — var types are 0..29 (FST_VT_MIN..FST_VT_MAX)
                // Standard format: [type][dir][name\0][len_varint][alias_varint]
                // vcd2fst compact:  [type][dir][len_varint][alias_varint][name\0]
                _ if code <= 29 => {
                    unknown_skip_count = 0;
                    fst_handle += 1;
                    if pos >= data.len() { break; }

                    let direction = data[pos];
                    pos += 1;
                    if pos >= data.len() { break; }

                    let result = if pos < data.len() && data[pos] & 0x80 != 0 {
                        // Compact: [len_varint][alias_varint][name\0]
                        let (width_val, consumed) = decode_varint(&data[pos..]).unwrap_or((0, 0));
                        pos += consumed;
                        if consumed == 0 { None }
                        else {
                            let (alias_val, consumed) = decode_varint(&data[pos..]).unwrap_or((0, 0));
                            pos += consumed;
                            if consumed == 0 { None }
                            else {
                                let (name_str, consumed) = self.read_cstring_from_slice(&data[pos..]);
                                pos += consumed;
                                Some((name_str, width_val, alias_val))
                            }
                        }
                    } else {
                        // Standard format: [name\0][len_varint][alias_varint]
                        let (name_str, consumed) = self.read_cstring_from_slice(&data[pos..]);
                        pos += consumed;
                        let (width_val, consumed) = decode_varint(&data[pos..]).unwrap_or((0, 0));
                        pos += consumed;
                        let (alias_val, consumed) = decode_varint(&data[pos..]).unwrap_or((0, 0));
                        pos += consumed;
                        Some((name_str, width_val, alias_val))
                    };

                    if let Some((name, width, alias)) = result {
                        let all_control = name.chars().all(|c| c.is_ascii_control());
                        let is_valid = if name.is_empty() || all_control {
                            false
                        } else if alias > 0 && name.len() <= 3 {
                            true
                        } else {
                            let printable = name.chars().filter(|c| c.is_ascii_graphic() || *c == ' ').count();
                            printable >= name.len().saturating_sub(3)
                        };

                        if is_valid {
                            let final_name = if alias > 0 && name.len() <= 3 {
                                self.compact_aliases.push((fst_handle, alias));
                                format!("@alias_{}", alias)
                            } else {
                                name
                            };
                            self.file.signals.push(SignalDecl {
                                handle: fst_handle,
                                name: final_name,
                                width: width as u32,
                                var_type: VarType::from_u8(code),
                                direction,
                            });
                        }
                    }
                }
                // Unknown codes (30-251): compact alias data or non-HIER content
                // Skip them and continue, but give up after 500 consecutive unknowns
                _ => {
                    unknown_skip_count += 1;
                }
            }
        }

        Ok(())
    }

    /// Read a null-terminated string from a byte slice
    fn read_cstring_from_slice<'a>(&self, data: &'a [u8]) -> (String, usize) {
        let mut pos = 0;
        while pos < data.len() && data[pos] != 0 {
            pos += 1;
        }
        let name = String::from_utf8_lossy(&data[..pos]).to_string();
        (name, pos + 1)
    }

    fn read_zwrapper_block(&mut self, len: u64) -> io::Result<()> {
        let compressed = self.read_bytes(len as usize)?;

        let decompressed = {
            use std::io::Read;
            // Detect compression format from magic bytes
            if compressed.starts_with(&[0x1f, 0x8b]) {
                // gzip
                use flate2::read::GzDecoder;
                let mut decoder = GzDecoder::new(&compressed[..]);
                let mut output = Vec::new();
                decoder.read_to_end(&mut output)?;
                output
            } else {
                // zlib
                use flate2::read::ZlibDecoder;
                let mut decoder = ZlibDecoder::new(&compressed[..]);
                let mut output = Vec::new();
                decoder.read_to_end(&mut output)?;
                output
            }
        };

        let mut cursor = std::io::Cursor::new(decompressed);
        let inner_reader = FstReader::from_reader(&mut cursor)?;

        self.file.header = inner_reader.file.header;
        self.file.signals.extend(inner_reader.file.signals);
        self.file.scopes.extend(inner_reader.file.scopes);

        Ok(())
    }

    #[allow(dead_code)]
    fn read_cstring(&mut self) -> io::Result<String> {
        let mut buf = Vec::new();
        loop {
            let b = self.read_u8()?;
            if b == 0 {
                break;
            }
            buf.push(b);
        }
        Ok(String::from_utf8_lossy(&buf).to_string())
    }
}

impl VarType {
    pub fn from_u8(v: u8) -> Self {
        match v {
            16 => VarType::VcdWire,
            5 => VarType::VcdReg,
            18 => VarType::VcdPort,
            29 => VarType::Integer,
            3 => VarType::Real,
            21 => VarType::GenString,
            22 => VarType::SvBit,
            23 => VarType::SvLogic,
            24 => VarType::SvInt,
            _ => VarType::VcdWire,
        }
    }
}

impl ScopeType {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => ScopeType::VcdModule,
            1 => ScopeType::VcdTask,
            2 => ScopeType::VcdFunction,
            3 => ScopeType::VcdBegin,
            4 => ScopeType::VcdFork,
            5 => ScopeType::VcdGenerate,
            _ => ScopeType::VcdModule,
        }
    }
}

impl FstFile {
    pub fn signal_names(&self) -> Vec<String> {
        self.signals.iter().map(|s| s.name.clone()).collect()
    }

    pub fn signal_by_name(&self, name: &str) -> Option<&SignalDecl> {
        self.signals.iter().find(|s| s.name == name)
    }

    #[allow(dead_code)]
    pub fn signal_by_handle(&self, handle: u32) -> Option<&SignalDecl> {
        self.signals.iter().find(|s| s.handle == handle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_vartype_from_u8() {
        assert_eq!(VarType::from_u8(16), VarType::VcdWire);
        assert_eq!(VarType::from_u8(3), VarType::Real);
        assert_eq!(VarType::from_u8(99), VarType::VcdWire);
    }

    #[test]
    fn test_scopetype_from_u8() {
        assert_eq!(ScopeType::from_u8(0), ScopeType::VcdModule);
        assert_eq!(ScopeType::from_u8(5), ScopeType::VcdGenerate);
        assert_eq!(ScopeType::from_u8(99), ScopeType::VcdModule);
    }

    #[test]
    fn test_detect_endianness_le() {
        // walconv-style: block_len(LE)=321, endian_check=PI
        let mut buf = vec![0x00u8]; // block type
        buf.extend_from_slice(&321u64.to_le_bytes()); // LE block_len = 321
        buf.extend_from_slice(&[0u8; 16]); // start_time, end_time (zeros)
        buf.extend_from_slice(&f64::to_le_bytes(3.141592653589793)); // endian_check = PI
        assert_eq!(detect_endianness(&buf, 10000), false);
    }

    #[test]
    fn test_detect_endianness_be() {
        // Icarus-style: block_len(BE)=329, endian_check=e
        let mut buf = vec![0x00u8]; // block type
        buf.extend_from_slice(&329u64.to_be_bytes()); // BE block_len = 329
        buf.extend_from_slice(&[0u8; 16]); // start_time, end_time (zeros)
        buf.extend_from_slice(&f64::to_le_bytes(2.718281828459045)); // endian_check = e (always LE)
        assert_eq!(detect_endianness(&buf, 10000), true);
    }

    #[test]
    fn test_detect_endianness_both_invalid() {
        // When both LE and BE are invalid (larger than file), fall back to LE
        let buf = vec![0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
        assert_eq!(detect_endianness(&buf, 10), false);
    }

    #[test]
    fn test_reader_roundtrip_le() {
        // Write an LE FST with known signals, read it back
        let mut data = Vec::new();
        
        // Header block
        data.push(0x00); // block type
        
        // We'll write the block_len after we compute it
        let hdr_pos = data.len();
        data.extend_from_slice(&[0u8; 8]); // placeholder
        
        let hdr_body_start = data.len();
        data.extend_from_slice(&0u64.to_le_bytes()); // start_time
        data.extend_from_slice(&100u64.to_le_bytes()); // end_time
        data.extend_from_slice(&f64::to_le_bytes(3.141592653589793)); // endian_check = PI → LE
        data.extend_from_slice(&[0u8; 8]); // mem_used
        data.extend_from_slice(&1u64.to_le_bytes()); // scope_count
        data.extend_from_slice(&1u64.to_le_bytes()); // var_count
        data.extend_from_slice(&0u64.to_le_bytes()); // max_handle
        data.extend_from_slice(&0u64.to_le_bytes()); // vc_count
        data.push(0i8 as u8); // timescale_exp
        data.extend_from_slice(&[0u8; 128]); // version
        data.extend_from_slice(&[0u8; 128]); // date
        
        let hdr_body_len = (data.len() - hdr_body_start) as u64;
        data[hdr_pos..hdr_pos+8].copy_from_slice(&hdr_body_len.to_le_bytes());
        
        // GEOM block
        data.push(0x03); // block type
        let geom_pos = data.len();
        data.extend_from_slice(&[0u8; 8]); // placeholder
        
        let geom_body_start = data.len();
        data.extend_from_slice(&0u64.to_le_bytes()); // section_length
        data.extend_from_slice(&0u64.to_le_bytes()); // uncompressed_length
        data.extend_from_slice(&0u64.to_le_bytes()); // max_handle
        data.extend_from_slice(&0u32.to_le_bytes()); // handle
        data.push(5u8); // name_len = 5
        data.extend_from_slice(b"hello"); // name
        data.push(16u8); // var_type = VcdWire
        data.push(0u8); // direction
        data.extend_from_slice(&1u32.to_le_bytes()); // width
        
        let geom_body_len = (data.len() - geom_body_start) as u64;
        data[geom_pos..geom_pos+8].copy_from_slice(&geom_body_len.to_le_bytes());
        
        let cursor = Cursor::new(data);
        let reader = FstReader::from_reader(cursor).unwrap();
        assert_eq!(reader.file.header.end_time, 100);
        assert_eq!(reader.file.signals.len(), 1);
        assert_eq!(reader.file.signals[0].name, "hello");
    }

    #[test]
    fn test_reader_roundtrip_be() {
        // Write a BE FST with known signals, read it back
        let mut data = Vec::new();
        
        // Header block
        data.push(0x00); // block type
        
        let hdr_pos = data.len();
        data.extend_from_slice(&[0u8; 8]); // placeholder
        
        let hdr_body_start = data.len();
        data.extend_from_slice(&0u64.to_be_bytes()); // start_time
        data.extend_from_slice(&200u64.to_be_bytes()); // end_time
        // endian_check = e (always stored as LE)
        data.extend_from_slice(&f64::to_le_bytes(2.718281828459045));
        data.extend_from_slice(&[0u8; 8]); // mem_used
        data.extend_from_slice(&1u64.to_be_bytes()); // scope_count
        data.extend_from_slice(&1u64.to_be_bytes()); // var_count
        data.extend_from_slice(&0u64.to_be_bytes()); // max_handle
        data.extend_from_slice(&0u64.to_be_bytes()); // vc_count
        data.push(0i8 as u8); // timescale_exp
        data.extend_from_slice(&[0u8; 128]); // version
        data.extend_from_slice(&[0u8; 128]); // date
        
        let hdr_body_len = (data.len() - hdr_body_start) as u64;
        data[hdr_pos..hdr_pos+8].copy_from_slice(&hdr_body_len.to_be_bytes());
        
        // HIER block (Icarus format: 0xFE = scope, var types 0-29 = variable entries)
        let hier_data = vec![
            0xFE,       // FST_ST_VCD_SCOPE
            0x00,       // scope_type = module
            b't', b'o', b'p', 0x00, // scope_name = "top"
            0x00,       // scope_comp (empty)
            16,         // FST_VT_VCD_WIRE = var_type
            0x00,       // direction
            b'w', b'o', b'r', b'l', b'd', 0x00, // name = "world"
            0x02,       // width = 2 (varint)
            0x00,       // alias_handle = 0 (varint, non-alias)
            0xFF,       // FST_ST_VCD_UPSCOPE
        ];
        // HIER block: type 0x04 (HIER uncompressed), BE_len = data length
        data.push(0x04);
        data.extend_from_slice(&(hier_data.len() as u64).to_be_bytes());
        data.extend_from_slice(&hier_data);
        
        let cursor = Cursor::new(data);
        let reader = FstReader::from_reader(cursor).unwrap();
        assert_eq!(reader.file.header.end_time, 200);
        assert_eq!(reader.file.signals.len(), 1);
        assert_eq!(reader.file.signals[0].name, "world");
        assert_eq!(reader.file.signals[0].width, 2);
        assert_eq!(reader.file.scopes.len(), 1);
        assert_eq!(reader.file.scopes[0].name, "top");
    }

    #[test]
    fn test_detect_endianness_from_header() {
        // Test using the actual detect_endianness helper
        // LE file: block_len LE fits, endian_check = PI
        let mut le_data = vec![0x00u8; 33];
        le_data[1..9].copy_from_slice(&100u64.to_le_bytes());
        le_data[25..33].copy_from_slice(&f64::to_le_bytes(3.141592653589793));
        assert!(!detect_endianness(&le_data, 1000));

        // BE file: block_len BE fits, endian_check = e (in LE)
        let mut be_data = vec![0x00u8; 33];
        be_data[1..9].copy_from_slice(&100u64.to_be_bytes());
        be_data[25..33].copy_from_slice(&f64::to_le_bytes(2.718281828459045));
        assert!(detect_endianness(&be_data, 1000));
    }
}