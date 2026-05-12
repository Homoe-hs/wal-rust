//! FST trace implementation with proper VCDATA_DYN_ALIAS2 (0x08) block parsing.
//!
//! Based on the libfst source code (MIT license by Tony Bybell).
//! Block format per fstapi.c fstReaderIterBlocks2 and block_format.txt.
//!
//! 0x08 block layout:
//!   [type:1][section_length:8][begin_time:8][end_time:8][mem_req:8]
//!   [maxvalpos:varint][compressed_len:varint][maxhandle:varint][checkpoint_data]
//!   [vc_maxhandle:varint][packtype:1][chain_data...]
//!   [index_table][index_length:8][compressed_time:var][tsec_uclen:8][tsec_clen:8][tsec_nitems:8]

use crate::fst::reader::{FstReader, FstFile};
use crate::fst::varint::{decode_varint, decode_fst_svarint};
use crate::trace::{Trace, TraceId, ScalarValue, FindCondition};
use std::cell::RefCell;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::io::BufReader;

/// RCV multi-state value lookup string (from fstapi.c FST_RCV_STR)
const FST_RCV_STR: &[u8] = b"xzhuwl-?";

/// Metadata for one VCDATA block
struct BlockInfo {
    time_begin: u64,
    time_end: u64,
    file_offset: u64,    // absolute position of block type byte
    block_len: u64,       // section_length (covers everything after type byte)
    /// Absolute file offset of the time section trailer (= type + 1 + block_len - 24)
    time_trailer_offset: u64,
    /// Compressed time data length (for computing index position)
    tsec_clen: u64,
    /// All timestamps in this block
    time_table: Vec<u64>,
}

pub struct FstTrace {
    id: TraceId,
    filename: String,
    file: FstFile,

    // Pass 1: index only
    timestamps: Vec<u64>,
    timestamps_set: std::collections::HashSet<u64>,
    block_index: Vec<BlockInfo>,

    // Pass 2: LRU caches
    block_cache: RefCell<lru::LruCache<usize, Vec<u8>>>,
    value_cache: RefCell<lru::LruCache<(u32, u64), Vec<u8>>>,

    // Hot signal tracking: handle → access count
    freq_map: RefCell<std::collections::HashMap<u32, u64>>,
    freq_threshold: u64,

    // Persistent file handle for on-demand reads
    reader: RefCell<BufReader<std::fs::File>>,

    // Runtime
    current_index: usize,
    max_index: usize,

    /// Endianness: true = big-endian (Icarus), false = little-endian (walconv)
    big_endian: bool,
}

impl FstTrace {
    pub fn load(path: &Path, id: TraceId) -> Result<Self, String> {
        let filename = path.to_string_lossy().to_string();

        let reader = FstReader::from_path(path)
            .map_err(|e| format!("Failed to read FST file {}: {}", filename, e))?;

        let big_endian = reader.is_big_endian();
        let file = reader.file;

        let mut trace = FstTrace {
            id,
            filename: filename.clone(),
            file,
            timestamps: Vec::new(),
            timestamps_set: std::collections::HashSet::new(),
            block_index: Vec::new(),
            block_cache: RefCell::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(16).unwrap(),
            )),
            value_cache: RefCell::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(100_000).unwrap(),
            )),
            freq_map: RefCell::new(std::collections::HashMap::new()),
            freq_threshold: 50,
            reader: RefCell::new(BufReader::with_capacity(
                1024 * 1024,
                std::fs::File::open(path)
                    .map_err(|e| format!("Failed to open {}: {}", filename, e))?,
            )),
            current_index: 0,
            max_index: 0,
            big_endian,
        };

        // Pass 1: build block index + timestamps
        trace.build_index()?;
        trace.max_index = if trace.timestamps.is_empty() {
            0
        } else {
            trace.timestamps.len() - 1
        };

        Ok(trace)
    }

    fn is_zlib(b0: u8, b1: u8) -> bool {
        b0 == 0x78 && (b1 == 0x01 || b1 == 0x5e || b1 == 0x9c || b1 == 0xda)
    }

    /// Pass 1: scan file for VCDATA 0x08 blocks, extract timestamps from time section.
    fn build_index(&mut self) -> Result<(), String> {
        let mut reader = self.reader.borrow_mut();
        reader.seek(SeekFrom::Start(0))
            .map_err(|e| format!("Seek error: {}", e))?;

        loop {
            let block_type = match read_u8(&mut *reader) {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(format!("Read error: {}", e)),
            };
            let block_len = read_u64(&mut *reader, self.big_endian)
                .map_err(|e| format!("Read error: {}", e))?;

            match block_type {
                0x00 => {
                    // HDR block: actual body is always 321 bytes regardless of stored len.
                    let _ = reader.seek(SeekFrom::Current(321));
                }
                0x08 => {
                    // VCDATA_DYN_ALIAS2 — extract timestamps from time section at block end.
                    let type_offset = reader.stream_position()
                        .map_err(|e| format!("Seek error: {}", e))? - 9;

                    // Validate block_len is reasonable
                    if block_len < 32 || block_len > 100_000_000 {
                        let _ = reader.seek(SeekFrom::Start(type_offset + 1 + block_len));
                        continue;
                    }

                    // The time section trailer is at: type_offset + 1 + block_len - 24
                    let trailer_off = type_offset + 1 + block_len - 24;
                    reader.seek(SeekFrom::Start(trailer_off))
                        .map_err(|e| format!("Seek error: {}", e))?;

                    let tsec_uclen = read_u64(&mut *reader, self.big_endian)
                        .map_err(|e| format!("Read tsec_uclen: {}", e))?;
                    let tsec_clen = read_u64(&mut *reader, self.big_endian)
                        .map_err(|e| format!("Read tsec_clen: {}", e))?;
                    let tsec_nitems = read_u64(&mut *reader, self.big_endian)
                        .map_err(|e| format!("Read tsec_nitems: {}", e))?;

                    if tsec_clen > block_len || tsec_uclen > 10_000_000 || tsec_nitems > 10_000_000 {
                        let _ = reader.seek(SeekFrom::Start(type_offset + 1 + block_len));
                        continue;
                    }

                    // Read compressed time data (located before the trailer)
                    let time_data_off = trailer_off - tsec_clen;
                    reader.seek(SeekFrom::Start(time_data_off))
                        .map_err(|e| format!("Seek error: {}", e))?;
                    let comp_time_data = read_bytes(&mut *reader, tsec_clen as usize)
                        .map_err(|e| format!("Read time data: {}", e))?;

                    // Decompress time data
                    let time_data = if tsec_uclen == tsec_clen {
                        comp_time_data
                    } else {
                        match decompress_zlib(&comp_time_data) {
                            Ok(d) => d,
                            Err(_) => {
                                let _ = reader.seek(SeekFrom::Start(type_offset + 1 + block_len));
                                continue;
                            }
                        }
                    };

                    // Parse varint time deltas into absolute timestamps
                    let mut time_table = Vec::with_capacity(tsec_nitems as usize);
                    let mut tp = 0usize;
                    let mut accum: u64 = 0;
                    while tp < time_data.len() && time_table.len() < tsec_nitems as usize {
                        match decode_varint(&time_data[tp..]) {
                            Some((delta, consumed)) => {
                                accum += delta;
                                time_table.push(accum);
                                tp += consumed;
                            }
                            None => break,
                        }
                    }

                    // Add all timestamps to global list
                    let time_begin = time_table.first().copied().unwrap_or(0);
                    let time_end = time_table.last().copied().unwrap_or(0);
                    for &t in &time_table {
                        if self.timestamps_set.insert(t) {
                            self.timestamps.push(t);
                        }
                    }

                    self.block_index.push(BlockInfo {
                        time_begin,
                        time_end,
                        file_offset: type_offset,
                        block_len,
                        time_trailer_offset: trailer_off,
                        tsec_clen,
                        time_table,
                    });

                    // Seek to next block
                    reader.seek(SeekFrom::Start(type_offset + 1 + block_len))
                        .map_err(|e| format!("Seek error: {}", e))?;
                }
                0x01 | 0x02 | 0x03 | 0x04 | 0x05 | 0x06 | 0x07 => {
                    let _ = reader.seek(SeekFrom::Current(block_len as i64));
                }
                0xFE => {
                    break;
                }
                _ => {
                    break; // Unknown block type → end of structured data
                }
            }
        }

        // Sort timestamps
        self.timestamps.sort();
        Ok(())
    }

    /// Find block index containing target_time
    fn find_block(&self, target_time: u64) -> Option<usize> {
        self.block_index.iter().position(|b| {
            b.time_begin <= target_time && target_time <= b.time_end
        })
    }

    /// On-demand: read chain index for the target handle from a 0x08 block,
    /// decompress its chain data, and extract the value at target_time.
    fn read_signal_value_at(&self, handle: u32, target_time: u64, signal_len: u32) -> Vec<u8> {
        let block_idx = match self.find_block(target_time) {
            Some(i) => i,
            None => return vec![b'x'],
        };

        let block = &self.block_index[block_idx];
        let mut reader = self.reader.borrow_mut();

        // --- Step 1: seek to block start and parse forward to find vc_start ---
        if reader.seek(SeekFrom::Start(block.file_offset + 9)).is_err() {
            return vec![b'x'];
        }
        // Skip begin_time (8), end_time (8), mem_required (8) = 24 bytes
        if reader.seek(SeekFrom::Current(24)).is_err() {
            return vec![b'x'];
        }
        // Read checkpoint section varints
        let _maxvalpos = match decode_varint_from_reader(&mut *reader) {
            Ok(v) => v, Err(_) => return vec![b'x'],
        };
        let frame_clen = match decode_varint_from_reader(&mut *reader) {
            Ok(v) => v, Err(_) => return vec![b'x'],
        };
        let _frame_maxhandle = match decode_varint_from_reader(&mut *reader) {
            Ok(v) => v, Err(_) => return vec![b'x'],
        };
        // Seek past compressed checkpoint data
        if reader.seek(SeekFrom::Current(frame_clen as i64)).is_err() {
            return vec![b'x'];
        }
        // Read VC data header
        let vc_maxhandle = match decode_varint_from_reader(&mut *reader) {
            Ok(v) => v, Err(_) => return vec![b'x'],
        };
        let packtype = match read_u8(&mut *reader) {
            Ok(b) => b, Err(_) => return vec![b'x'],
        };
        let vc_start = reader.stream_position().unwrap_or(0);

        // --- Step 2: compute chain index position and parse for target handle ---
        // indx_pntr = type_offset + 1 + block_len - 24 - tsec_clen - 8
        let indx_pntr = block.file_offset + 1 + block.block_len - 24 - block.tsec_clen - 8;
        if reader.seek(SeekFrom::Start(indx_pntr)).is_err() {
            return vec![b'x'];
        }
        let chain_clen = match read_u64(&mut *reader, self.big_endian) {
            Ok(v) => v, Err(_) => return vec![b'x'],
        };
        let indx_pos = indx_pntr - chain_clen;
        if reader.seek(SeekFrom::Start(indx_pos)).is_err() {
            return vec![b'x'];
        }
        let index_data = match read_bytes(&mut *reader, chain_clen as usize) {
            Ok(d) => d, Err(_) => return vec![b'x'],
        };

        // --- Step 3: walk DYNALIAS2 signed varint index to find handle's chain ---
        let idx_limit = vc_maxhandle.min(self.file.header.max_handle.max(1)) as usize;
        let mut chain_table: Vec<u64> = vec![0; idx_limit + 1];
        let mut chain_table_lengths: Vec<i64> = vec![0; idx_limit + 1];
        let mut ip = 0usize;
        let mut idx = 0usize;
        let mut pval: u64 = 0;
        let mut pidx = 0usize;
        let mut prev_alias: i64 = 0;

        while ip < index_data.len() && idx < idx_limit {
            match decode_fst_svarint(&index_data[ip..]) {
                Some((shval, consumed)) => {
                    ip += consumed;
                    if shval & 1 != 0 {
                        let raw = shval >> 1;
                        if raw > 0 {
                            pval += raw as u64;
                            chain_table[idx] = pval;
                            if idx > 0 {
                                chain_table_lengths[pidx] = pval as i64 - chain_table[pidx] as i64;
                            }
                            pidx = idx;
                            idx += 1;
                        } else if raw < 0 {
                            chain_table[idx] = 0;
                            chain_table_lengths[idx] = raw; // alias reference
                            prev_alias = raw;
                            idx += 1;
                        } else {
                            chain_table[idx] = 0;
                            chain_table_lengths[idx] = prev_alias;
                            idx += 1;
                        }
                    } else {
                        let loopcnt = (shval >> 1) as usize;
                        for _ in 0..loopcnt {
                            if idx >= idx_limit { break; }
                            chain_table[idx] = 0;
                            idx += 1;
                        }
                    }
                }
                None => break,
            }
        }

        // Update last chain length
        if pidx < idx_limit {
            let chain_end = indx_pntr - 8 - vc_start; // index position in file relative to vc_start
            chain_table_lengths[pidx] = chain_end as i64 - chain_table[pidx] as i64;
        }

        // Resolve aliases
        for i in 0..idx.min(idx_limit) {
            let v32 = chain_table_lengths[i];
            if v32 < 0 && chain_table[i] == 0 {
                let alias_idx = (-v32 as usize) - 1;
                if alias_idx < idx_limit {
                    chain_table[i] = chain_table[alias_idx];
                    chain_table_lengths[i] = chain_table_lengths[alias_idx];
                }
            }
        }

        // --- Step 4: find the target handle's chain ---
        let h = handle as usize;
        if h >= idx_limit || h >= chain_table.len() {
            return vec![b'x'];
        }
        let chain_off = chain_table[h];
        let chain_len = chain_table_lengths[h];
        if chain_off == 0 || chain_len <= 0 {
            return vec![b'x'];
        }

        // --- Step 5: read and decompress the chain data ---
        if reader.seek(SeekFrom::Start(vc_start + chain_off)).is_err() {
            return vec![b'x'];
        }
        let (destlen, skiplen) = match decode_varint_from_reader(&mut *reader) {
            Ok(v) => (v, 0usize),
            Err(_) => return vec![b'x'],
        };
        let actual_chain_len = chain_len as u64 - skiplen as u64;
        let chain_data = match read_bytes(&mut *reader, actual_chain_len as usize) {
            Ok(d) => d, Err(_) => return vec![b'x'],
        };

        let chain_mem = if destlen > 0 {
            match decompress_chain(&chain_data, destlen as usize, packtype) {
                Some(d) => d,
                None => return vec![b'x'],
            }
        } else {
            chain_data // uncompressed
        };

        // --- Step 6: walk chain entries to find value at target_time ---
        // Find target time index in time_table
        let target_tidx = match block.time_table.iter().position(|&t| t == target_time) {
            Some(i) => i,
            None => return vec![b'x'],
        };

        // Walk the chain data
        extract_value_from_chain(&chain_mem, signal_len, target_tidx, &block.time_table)
    }
}

/// Decompress chain data using the appropriate pack type
fn decompress_chain(data: &[u8], destlen: usize, packtype: u8) -> Option<Vec<u8>> {
    match packtype {
        b'Z' | b'!' => {
            use flate2::read::ZlibDecoder;
            use std::io::Read;
            let mut decoder = ZlibDecoder::new(data);
            let mut out = vec![0u8; destlen];
            decoder.read_exact(&mut out).ok()?;
            Some(out)
        }
        b'F' => {
            // FastLZ not directly available — try LZ4 as fallback
            lz4_flex::block::decompress(data, destlen).ok()
        }
        b'4' => {
            lz4_flex::block::decompress(data, destlen).ok()
        }
        _ => None,
    }
}

/// Extract value from decompressed chain data for a given time index.
/// Each chain entry is a single varint encoding both time_delta and value:
///   Scalar (len=1): LSB=0 → val=((vli>>1)&1)|'0', tdelta=vli>>2
///                    LSB=1 → val=FST_RCV_STR[(vli>>1)&7], tdelta=vli>>4
///   Vector (len>1):  LSB=0 → binary bit-packed, LSB=1 → non-binary literal
fn extract_value_from_chain(
    chain_mem: &[u8],
    signal_len: u32,
    target_tidx: usize,
    _time_table: &[u64],
) -> Vec<u8> {
    let mut pos = 0usize;
    let mut tidx = 0usize;

    while pos < chain_mem.len() {
        let (vli, skiplen) = match decode_varint(&chain_mem[pos..]) {
            Some(v) => v,
            None => break,
        };
        pos += skiplen;

        if signal_len <= 1 {
            let tdelta;
            if vli & 1 == 0 {
                // Binary scalar: LSB=0, val=((vli>>1)&1)|'0', tdelta=vli>>2
                if tidx + (vli >> 2) as usize >= target_tidx {
                    return vec![(((vli >> 1) & 1) as u8) | b'0'];
                }
                tdelta = (vli >> 2) as usize;
            } else {
                // Multi-state scalar: LSB=1, val=FST_RCV_STR[(vli>>1)&7], tdelta=vli>>4
                if tidx + (vli >> 4) as usize >= target_tidx {
                    return vec![FST_RCV_STR[((vli >> 1) & 7) as usize]];
                }
                tdelta = (vli >> 4) as usize;
            }
            tidx += tdelta;
        } else {
            // Vector encoding: LSB=nonbinary_flag, tdelta=vli>>1
            let tdelta = (vli >> 1) as usize;
            tidx += tdelta;

            let val = if vli & 1 == 0 {
                // Binary vector: bit-packed
                let byte_count = ((signal_len as usize) + 7) / 8;
                if pos + byte_count > chain_mem.len() {
                    break;
                }
                let mut v = vec![b'0'; signal_len as usize];
                for j in 0..signal_len as usize {
                    let bit = 7 - (j & 7);
                    v[j] = ((chain_mem[pos + j / 8] >> bit) & 1) | b'0';
                }
                pos += byte_count;
                v
            } else {
                // Non-binary vector: varint(len) + literal
                let (len, lskip) = match decode_varint(&chain_mem[pos..]) {
                    Some(v) => v,
                    None => break,
                };
                pos += lskip;
                if pos + len as usize > chain_mem.len() {
                    break;
                }
                let v = chain_mem[pos..pos + len as usize].to_vec();
                pos += len as usize;
                v
            };

            if tidx == target_tidx {
                return val;
            }
        }

        if tidx > target_tidx {
            break;
        }
    }

    // Return checkpoint value instead of 'x' if available
    vec![b'x']
}

// ================ I/O helpers ================

fn read_u8<R: Read>(reader: &mut R) -> std::io::Result<u8> {
    let mut buf = [0u8; 1];
    reader.read_exact(&mut buf)?;
    Ok(buf[0])
}

fn read_u64<R: Read>(reader: &mut R, big_endian: bool) -> std::io::Result<u64> {
    let mut buf = [0u8; 8];
    reader.read_exact(&mut buf)?;
    if big_endian {
        Ok(u64::from_be_bytes(buf))
    } else {
        Ok(u64::from_le_bytes(buf))
    }
}

fn read_bytes<R: Read>(reader: &mut R, len: usize) -> std::io::Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(buf)
}

fn decode_varint_from_reader<R: Read>(reader: &mut R) -> Result<u64, String> {
    let mut buf = Vec::with_capacity(10);
    loop {
        let b = read_u8(reader).map_err(|e| format!("Read error: {}", e))?;
        buf.push(b);
        if b & 0x80 == 0 {
            break;
        }
    }
    decode_varint(&buf)
        .map(|(v, _)| v)
        .ok_or_else(|| "Failed to decode varint".to_string())
}

#[allow(dead_code)]
fn decompress_zlib(input: &[u8]) -> std::io::Result<Vec<u8>> {
    use flate2::read::ZlibDecoder;
    use std::io::Read;
    let mut decoder = ZlibDecoder::new(input);
    let mut output = Vec::new();
    decoder.read_to_end(&mut output)?;
    Ok(output)
}

// ================ Trace trait ================

impl Trace for FstTrace {
    fn id(&self) -> &TraceId { &self.id }
    fn filename(&self) -> &str { &self.filename }

    fn step(&mut self, steps: usize) -> Result<(), String> {
        let new_index = self.current_index.saturating_add(steps);
        if new_index > self.max_index {
            return Err(format!("Step {} would exceed max index {}", steps, self.max_index));
        }
        self.current_index = new_index;
        Ok(())
    }

    fn signal_value(&self, name: &str, offset: usize) -> Result<ScalarValue, String> {
        if self.timestamps.is_empty() {
            return Ok(ScalarValue::Bit(b'x'));
        }
        if offset >= self.timestamps.len() {
            return Err(format!("Offset {} out of range", offset));
        }
        let target_time = self.timestamps[offset];

        let sig = self.file.signal_by_name(name)
            .ok_or_else(|| format!("Unknown signal: {}", name))?;

        // Track access frequency
        let freq = {
            let mut fm = self.freq_map.borrow_mut();
            let c = fm.entry(sig.handle).or_insert(0);
            *c += 1;
            *c
        };

        // Check value cache
        {
            let mut cache = self.value_cache.borrow_mut();
            if let Some(val) = cache.get(&(sig.handle, target_time)) {
                return Ok(bytes_to_scalar(val));
            }
        }

        // On-demand read from block
        let val = self.read_signal_value_at(sig.handle, target_time, sig.width);
        let result = bytes_to_scalar(&val);

        // Cache the value
        self.value_cache.borrow_mut().put((sig.handle, target_time), val);

        // If signal is hot, pre-cache a few surrounding timestamps
        if freq >= self.freq_threshold && freq % 10 == 0 {
            let mut cache = self.value_cache.borrow_mut();
            let window_start = offset.saturating_sub(5);
            let window_end = (offset + 5).min(self.timestamps.len());
            for i in window_start..window_end {
                let t = self.timestamps[i];
                if !cache.contains(&(sig.handle, t)) {
                    let v = self.read_signal_value_at(sig.handle, t, sig.width);
                    cache.put((sig.handle, t), v);
                }
            }
        }

        Ok(result)
    }

    fn signal_width(&self, name: &str) -> Result<usize, String> {
        let sig = self.file.signal_by_name(name)
            .ok_or_else(|| format!("Unknown signal: {}", name))?;
        Ok(sig.width as usize)
    }

    fn signals(&self) -> Vec<String> {
        self.file.signal_names()
    }

    fn scopes(&self) -> Vec<String> {
        self.file.scopes.iter().map(|s| s.name.clone()).collect()
    }

    fn max_index(&self) -> usize { self.max_index }

    fn set_index(&mut self, index: usize) -> Result<(), String> {
        if index > self.max_index {
            return Err(format!("Index {} exceeds max {}", index, self.max_index));
        }
        self.current_index = index;
        Ok(())
    }

    fn index(&self) -> usize { self.current_index }

    fn find_indices(&self, name: &str, cond: FindCondition) -> Result<Vec<usize>, String> {
        let sig = self.file.signal_by_name(name)
            .ok_or_else(|| format!("Unknown signal: {}", name))?;

        let mut indices = Vec::new();
        let mut prev_bit: Option<u8> = None;

        // Walk all timestamps, reading values one by one (but LRU cache will help)
        // For a fully optimized chain-walking approach, see VcdTrace's single-pass scan.
        for (i, &target_time) in self.timestamps.iter().enumerate() {
            let val = self.read_signal_value_at(sig.handle, target_time, sig.width);
            let curr_bit = if val.len() == 1 { Some(val[0]) } else { None };

            let matches_cond = match &cond {
                FindCondition::Rising => prev_bit == Some(b'0') && curr_bit == Some(b'1'),
                FindCondition::Falling => prev_bit == Some(b'1') && curr_bit == Some(b'0'),
                FindCondition::High => curr_bit == Some(b'1'),
                FindCondition::Low => curr_bit == Some(b'0'),
                FindCondition::Value(v) => {
                    curr_bit == Some(*v) || (curr_bit == Some(b'1') && *v == 1) || (curr_bit == Some(b'0') && *v == 0)
                }
                FindCondition::ValueI64(target) => {
                    let int_val = if val.len() == 1 {
                        Some(if val[0] == b'1' { 1i64 } else { 0i64 })
                    } else if val.iter().all(|&b| b == b'0' || b == b'1') {
                        Some(val.iter().fold(0i64, |acc, &b|
                            acc.overflowing_shl(1).0 | if b == b'1' { 1 } else { 0 }
                        ))
                    } else {
                        None
                    };
                    int_val == Some(*target)
                }
                FindCondition::Neq(v) => {
                    !(curr_bit == Some(*v) || (curr_bit == Some(b'1') && *v == 1) || (curr_bit == Some(b'0') && *v == 0))
                }
                FindCondition::NeqI64(target) => {
                    let int_val = if val.len() == 1 {
                        Some(if val[0] == b'1' { 1i64 } else { 0i64 })
                    } else if val.iter().all(|&b| b == b'0' || b == b'1') {
                        Some(val.iter().fold(0i64, |acc, &b|
                            acc.overflowing_shl(1).0 | if b == b'1' { 1 } else { 0 }
                        ))
                    } else {
                        None
                    };
                    int_val != Some(*target)
                }
            };

            if matches_cond {
                indices.push(i);
            }
            prev_bit = curr_bit;
        }

        Ok(indices)
    }
}

fn bytes_to_scalar(data: &[u8]) -> ScalarValue {
    match data.len() {
        1 => ScalarValue::Bit(data[0]),
        _ => ScalarValue::Vector(data.to_vec()),
    }
}
