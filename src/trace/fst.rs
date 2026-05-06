//! FST trace implementation (two-pass index + LRU cache)
//!
//! Pass 1: build block index + timestamps (no data storage)
//! Pass 2: on-demand LZ4 block decompression + LRU cache

use crate::fst::reader::{FstReader, FstFile};
use crate::fst::varint::decode_varint;
use crate::trace::{Trace, TraceId, ScalarValue, FindCondition};
use std::cell::RefCell;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::io::BufReader;

const BLOCK_CACHE_SIZE: usize = 16; // cache up to 16 decompressed blocks

/// Metadata for one VCDATA block
struct BlockInfo {
    time_begin: u64,
    time_end: u64,
    file_offset: u64,
}

pub struct FstTrace {
    id: TraceId,
    filename: String,
    file: FstFile,

    // Pass 1: index only
    timestamps: Vec<u64>,
    block_index: Vec<BlockInfo>,

    // Pass 2: LRU caches
    block_cache: RefCell<lru::LruCache<usize, Vec<u8>>>,
    value_cache: RefCell<lru::LruCache<(u32, u64), Vec<u8>>>,

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
            block_index: Vec::new(),
            block_cache: RefCell::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(BLOCK_CACHE_SIZE).unwrap(),
            )),
            value_cache: RefCell::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(100_000).unwrap(),
            )),
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

    /// Pass 1: scan file for VCDATA blocks, record file positions and timestamps
    fn build_index(&mut self) -> Result<(), String> {
        let mut reader = self.reader.borrow_mut();

        // Seek back to start (FstReader may have consumed the file)
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
                0x01 => {
                    let start_pos = reader.stream_position()
                        .map_err(|e| format!("Seek error: {}", e))?;

                    // Read time_begin and time_end from the block header
                    let time_begin = decode_varint_from_reader(&mut *reader)?;
                    let time_end = decode_varint_from_reader(&mut *reader)?;

                    // Record block info
                    self.block_index.push(BlockInfo {
                        time_begin,
                        time_end,
                        file_offset: start_pos,
                    });

                    // Record timestamps
                    if !self.timestamps.contains(&time_begin) {
                        self.timestamps.push(time_begin);
                    }
                    if !self.timestamps.contains(&time_end) {
                        self.timestamps.push(time_end);
                    }

                    // Seek past this block
                    let end_pos = start_pos + block_len;
                    reader.seek(SeekFrom::Start(end_pos))
                        .map_err(|e| format!("Seek error: {}", e))?;
                }
                0xFE => {
                    // ZWRAPPER contains compressed FST data — skip it entirely.
                    // The inner data is already processed by FstReader.
                    drop(reader);
                    // Re-open file and seek past the entire ZWRAPPER block
                    let mut f = std::fs::File::open(&self.filename)
                        .map_err(|e| format!("Failed to reopen {}: {}", self.filename, e))?;
                    // The 0xFE block_len includes the compressed data already consumed.
                    // We don't know the exact position, so reopen from start and seek
                    // to the end of the file (all ZWRAPPER data has been consumed).
                    let file_len = f.seek(SeekFrom::End(0))
                        .map_err(|e| format!("Seek error: {}", e))?;
                    self.reader = RefCell::new(BufReader::with_capacity(
                        1024 * 1024,
                        std::fs::File::open(&self.filename)
                            .map_err(|e| format!("Failed to reopen {}: {}", self.filename, e))?,
                    ));
                    reader = self.reader.borrow_mut();
                    // Seek to end — there's nothing more to process after ZWRAPPER
                    reader.seek(SeekFrom::Start(file_len)).ok();
                    break;
                }
                _ => {
                    reader.seek(SeekFrom::Current(block_len as i64))
                        .map_err(|e| format!("Seek error: {}", e))?;
                }
            }
        }

        Ok(())
    }

    /// Find block index containing target_time
    fn find_block(&self, target_time: u64) -> Option<usize> {
        self.block_index.iter().position(|b| {
            b.time_begin <= target_time && target_time <= b.time_end
        })
    }

    /// On-demand: decompress a block and find the last value of a signal at target_time
    fn read_signal_value_at(&self, handle: u32, target_time: u64) -> Vec<u8> {
        let block_idx = match self.find_block(target_time) {
            Some(i) => i,
            None => return vec![b'x'],
        };

        // Check block cache
        {
            let mut cache = self.block_cache.borrow_mut();
            if let Some(data) = cache.get(&block_idx) {
                return scan_block_value(data, handle, target_time);
            }
        }

        // Decompress the block
        let mut reader = self.reader.borrow_mut();
        let block = &self.block_index[block_idx];

        if let Err(_) = reader.seek(SeekFrom::Start(block.file_offset)) {
            return vec![b'x'];
        }

        let _time_begin = match decode_varint_from_reader(&mut *reader) {
            Ok(t) => t, Err(_) => return vec![b'x'],
        };
        let _time_end = decode_varint_from_reader(&mut *reader).unwrap_or(0);
        let _mem_required = decode_varint_from_reader(&mut *reader).unwrap_or(0);
        let compressed_len = decode_varint_from_reader(&mut *reader).unwrap_or(0);
        let _max_handle = decode_varint_from_reader(&mut *reader).unwrap_or(0);

        let compressed_data = match read_bytes(&mut *reader, compressed_len as usize) {
            Ok(d) => d,
            Err(_) => return vec![b'x'],
        };

        let decompressed = match lz4_flex::block::decompress_size_prepended(&compressed_data) {
            Ok(d) => d,
            Err(_) => return vec![b'x'],
        };

        // Cache the decompressed block
        {
            let mut cache = self.block_cache.borrow_mut();
            cache.put(block_idx, decompressed.clone());
        }

        scan_block_value(&decompressed, handle, target_time)
    }

    /// Decode all timestamps from a block and return them
    #[allow(dead_code)]
    fn collect_block_timestamps(&self, block_idx: usize) -> Vec<u64> {
        let block = &self.block_index[block_idx];
        let mut reader = self.reader.borrow_mut();

        if reader.seek(SeekFrom::Start(block.file_offset)).is_err() {
            return vec![];
        }

        let time_begin = match decode_varint_from_reader(&mut *reader) {
            Ok(t) => t, Err(_) => return vec![],
        };
        let _time_end = decode_varint_from_reader(&mut *reader).unwrap_or(0);
        let _mem_required = decode_varint_from_reader(&mut *reader).unwrap_or(0);
        let compressed_len = decode_varint_from_reader(&mut *reader).unwrap_or(0);
        let _max_handle = decode_varint_from_reader(&mut *reader).unwrap_or(0);

        let compressed_data = match read_bytes(&mut *reader, compressed_len as usize) {
            Ok(d) => d, Err(_) => return vec![],
        };

        let decompressed = match lz4_flex::block::decompress_size_prepended(&compressed_data) {
            Ok(d) => d, Err(_) => return vec![],
        };

        let mut timestamps = vec![time_begin];
        let mut pos = 0;
        let mut current_time = time_begin;

        while pos < decompressed.len() {
            let (delta, consumed) = match decode_varint(&decompressed[pos..]) {
                Some(v) => v,
                None => break,
            };
            pos += consumed;
            if delta == 0 { break; }
            current_time += delta;
            if !timestamps.contains(&current_time) {
                timestamps.push(current_time);
            }
            // Skip handles and values
            while pos < decompressed.len() {
                let (handle, c) = match decode_varint(&decompressed[pos..]) {
                    Some(v) => v, None => break,
                };
                pos += c;
                if handle == 0 { break; }
                if pos < decompressed.len() {
                    let len = decompressed[pos] as usize;
                    pos += 1;
                    if pos + len <= decompressed.len() {
                        pos += len;
                    }
                }
            }
        }

        timestamps
    }
}

/// Scan decompressed block data for the last value of a signal at target_time
fn scan_block_value(data: &[u8], target_handle: u32, target_time: u64) -> Vec<u8> {
    let mut pos = 0;
    let mut current_time: u64 = 0;
    let mut last_value: Option<Vec<u8>> = None;

    // Find the first time_delta to get start_time
    if pos < data.len() {
        let (first_delta, consumed) = match decode_varint(&data[pos..]) {
            Some(v) => v, None => return vec![b'x'],
        };
        pos += consumed;
        current_time = first_delta; // first delta IS the start time
    }

    while pos < data.len() {
        let (time_delta, consumed) = match decode_varint(&data[pos..]) {
            Some(v) => v, None => break,
        };
        pos += consumed;
        if time_delta == 0 { break; }
        current_time += time_delta;

        while pos < data.len() {
            let (handle, c) = match decode_varint(&data[pos..]) {
                Some(v) => v, None => break,
            };
            pos += c;
            if handle == 0 { break; }

            if pos < data.len() {
                let len = data[pos] as usize;
                pos += 1;
                if pos + len > data.len() { break; }
                let value = data[pos..pos + len].to_vec();
                pos += len;

                if current_time <= target_time && handle as u32 == target_handle {
                    last_value = Some(value);
                }
            }
        }
    }

    last_value.unwrap_or_else(|| vec![b'x'])
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

    fn load(path: &Path) -> Result<Self, String> where Self: Sized {
        Self::load(path, "default".to_string())
    }

    fn unload(&mut self) {
        self.timestamps.clear();
        self.block_index.clear();
        self.block_cache.borrow_mut().clear();
        self.value_cache.borrow_mut().clear();
    }

    fn step(&mut self, steps: usize) -> Result<(), String> {
        let new_index = self.current_index.saturating_add(steps);
        if new_index > self.max_index {
            return Err(format!("Step {} would exceed max index {}", steps, self.max_index));
        }
        self.current_index = new_index;
        Ok(())
    }

    fn signal_value(&self, name: &str, offset: usize) -> Result<ScalarValue, String> {
        if offset >= self.timestamps.len() {
            return Err(format!("Offset {} out of range", offset));
        }
        let target_time = self.timestamps[offset];

        let sig = self.file.signal_by_name(name)
            .ok_or_else(|| format!("Unknown signal: {}", name))?;

        // Check value cache
        {
            let mut cache = self.value_cache.borrow_mut();
            if let Some(val) = cache.get(&(sig.handle, target_time)) {
                return Ok(bytes_to_scalar(val));
            }
        }

        // On-demand read from block
        let val = self.read_signal_value_at(sig.handle, target_time);
        let result = bytes_to_scalar(&val);

        // Cache the value
        self.value_cache.borrow_mut().put((sig.handle, target_time), val);

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
        let mut prev_value: Option<u8> = None;

        for (i, &target_time) in self.timestamps.iter().enumerate() {
            let val = self.read_signal_value_at(sig.handle, target_time);
            let curr_value = if val.len() == 1 { Some(val[0]) } else { None };

            let matches = match (&cond, prev_value, curr_value) {
                (FindCondition::Rising, Some(0), Some(1)) => true,
                (FindCondition::Falling, Some(1), Some(0)) => true,
                (FindCondition::High, _, Some(1)) => true,
                (FindCondition::Low, _, Some(0)) => true,
                (FindCondition::Value(v), _, Some(val)) => val == *v,
                _ => false,
            };

            if matches { indices.push(i); }
            prev_value = curr_value;
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
