//! FST trace implementation

use crate::fst::reader::{FstReader, FstFile};
use crate::fst::varint::decode_varint;
use crate::trace::{Trace, TraceId, ScalarValue, FindCondition};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::io::BufReader;

pub struct FstTrace {
    id: TraceId,
    filename: String,
    file: FstFile,
    signal_data: HashMap<u32, Vec<(u64, Vec<u8>)>>,
    timestamps: Vec<u64>,
    current_index: usize,
    max_index: usize,
}

impl FstTrace {
    pub fn load(path: &Path, id: TraceId) -> Result<Self, String> {
        let filename = path.to_string_lossy().to_string();

        let reader = FstReader::from_path(path)
            .map_err(|e| format!("Failed to read FST file {}: {}", filename, e))?;

        let file = reader.file;
        let signals = file.signals.clone();

        let mut signal_data: HashMap<u32, Vec<(u64, Vec<u8>)>> = HashMap::new();
        for sig in &signals {
            signal_data.insert(sig.handle, Vec::new());
        }

        let mut timestamps = Vec::new();
        let mut max_index = 0;

        let mut trace = FstTrace {
            id,
            filename,
            file,
            signal_data,
            timestamps,
            current_index: 0,
            max_index,
        };

        // Load VCDATA blocks (waveform data)
        trace.read_vcdata_blocks(path)?;

        Ok(trace)
    }

    pub fn read_vcdata_blocks(&mut self, path: &Path) -> Result<(), String> {
        let filename = path.to_string_lossy().to_string();

        let reader = std::fs::File::open(path)
            .map_err(|e| format!("Failed to open {}: {}", filename, e))?;

        let mut reader = BufReader::with_capacity(1024 * 1024, reader);

        loop {
            let block_type = match read_u8(&mut reader) {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(format!("Read error: {}", e)),
            };

            let block_len = read_u64(&mut reader).map_err(|e| format!("Read error: {}", e))?;

            match block_type {
                0x01 => {
                    self.parse_vcdata_block(&mut reader, block_len)?;
                }
                0xFE => {
                    let compressed = read_bytes(&mut reader, block_len as usize).map_err(|e| format!("Read error: {}", e))?;
                    let decompressed = decompress_zlib(&compressed)
                        .map_err(|e| format!("Zlib decompression failed: {}", e))?;
                    let mut cursor = std::io::Cursor::new(decompressed);
                    self.parse_vcdata_from_reader(&mut cursor)?;
                }
                _ => {
                    reader.seek(SeekFrom::Current(block_len as i64)).map_err(|e| format!("Seek error: {}", e))?;
                }
            }
        }

        if !self.timestamps.is_empty() {
            self.max_index = self.timestamps.len() - 1;
        }

        Ok(())
    }

    fn parse_vcdata_block<R: Read + Seek>(&mut self, reader: &mut R, len: u64) -> Result<(), String> {
        let start_pos = reader.stream_position().map_err(|e| format!("Seek error: {}", e))?;
        let end_pos = start_pos + len;

        let time_begin = decode_varint_from_reader(reader)?;
        let time_end = decode_varint_from_reader(reader)?;
        let _mem_required = decode_varint_from_reader(reader)?;
        let compressed_len = decode_varint_from_reader(reader)?;
        let max_handle = decode_varint_from_reader(reader)?;

        let compressed_data = read_bytes(reader, compressed_len as usize).map_err(|e| format!("Read error: {}", e))?;

        let decompressed = lz4_flex::block::decompress_size_prepended(&compressed_data)
            .map_err(|e| format!("LZ4 decompression failed: {}", e))?;

        self.decode_vcdata(&decompressed, time_begin, max_handle)?;

        if reader.stream_position().map_err(|e| format!("Seek error: {}", e))? != end_pos {
            reader.seek(SeekFrom::Start(end_pos)).map_err(|e| format!("Seek error: {}", e))?;
        }

        if !self.timestamps.contains(&time_begin) {
            self.timestamps.push(time_begin);
        }
        if !self.timestamps.contains(&time_end) {
            self.timestamps.push(time_end);
        }

        Ok(())
    }

    fn decode_vcdata(&mut self, data: &[u8], start_time: u64, _max_handle: u64) -> Result<(), String> {
        let mut pos = 0;
        let mut current_time = start_time;

        while pos < data.len() {
            let (time_delta, consumed) = decode_varint(&data[pos..])
                .ok_or_else(|| format!("Failed to decode time delta at pos {}", pos))?;
            pos += consumed;

            if time_delta == 0 && pos < data.len() {
                break;
            }

            current_time += time_delta;

            // Record this timestamp if it's new
            if !self.timestamps.contains(&current_time) {
                self.timestamps.push(current_time);
            }

            while pos < data.len() {
                let handle = match decode_varint(&data[pos..]) {
                    Some((h, c)) => { pos += c; h }
                    None => break,
                };

                if handle == 0 {
                    break;
                }

                let len = data[pos] as usize;
                pos += 1;

                if pos + len > data.len() {
                    break;
                }

                let value = data[pos..pos + len].to_vec();
                pos += len;

                if let Some(changes) = self.signal_data.get_mut(&(handle as u32)) {
                    changes.push((current_time, value));
                }
            }
        }

        Ok(())
    }

    fn parse_vcdata_from_reader<R: Read + Seek>(&mut self, reader: &mut R) -> Result<(), String> {
        loop {
            let block_type = match read_u8(reader) {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(format!("Read error: {}", e)),
            };

            let block_len = read_u64(reader).map_err(|e| format!("Read error: {}", e))?;

            match block_type {
                0x01 => {
                    self.parse_vcdata_block(reader, block_len)?;
                }
                _ => {
                    reader.seek(SeekFrom::Current(block_len as i64)).map_err(|e| format!("Seek error: {}", e))?;
                }
            }
        }
        Ok(())
    }
}

fn read_u8<R: Read>(reader: &mut R) -> std::io::Result<u8> {
    let mut buf = [0u8; 1];
    reader.read_exact(&mut buf)?;
    Ok(buf[0])
}

fn read_u64<R: Read>(reader: &mut R) -> std::io::Result<u64> {
    let mut buf = [0u8; 8];
    reader.read_exact(&mut buf)?;
    Ok(u64::from_le_bytes(buf))
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

fn decompress_zlib(input: &[u8]) -> std::io::Result<Vec<u8>> {
    use flate2::read::ZlibDecoder;
    use std::io::Read;
    let mut decoder = ZlibDecoder::new(input);
    let mut output = Vec::new();
    decoder.read_to_end(&mut output)?;
    Ok(output)
}

impl Trace for FstTrace {
    fn id(&self) -> &TraceId {
        &self.id
    }

    fn filename(&self) -> &str {
        &self.filename
    }

    fn load(path: &Path) -> Result<Self, String>
    where
        Self: Sized,
    {
        Self::load(path, "default".to_string())
    }

    fn unload(&mut self) {
        self.signal_data.clear();
        self.timestamps.clear();
    }

    fn step(&mut self, steps: usize) -> Result<(), String> {
        let new_index = self.current_index.saturating_add(steps);
        if new_index > self.max_index {
            return Err(format!(
                "Step {} would exceed max index {}",
                steps, self.max_index
            ));
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

        let changes = self.signal_data.get(&sig.handle)
            .ok_or_else(|| format!("No data for signal: {}", name))?;

        let mut last_value: Option<&Vec<u8>> = None;
        for &(time, ref val) in changes {
            if time <= target_time {
                last_value = Some(val);
            } else {
                break;
            }
        }

        match last_value {
            Some(v) if v.len() == 1 => Ok(ScalarValue::Bit(v[0])),
            Some(v) => Ok(ScalarValue::Vector(v.clone())),
            None => Ok(ScalarValue::Bit(b'x')),
        }
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

    fn max_index(&self) -> usize {
        self.max_index
    }

    fn set_index(&mut self, index: usize) -> Result<(), String> {
        if index > self.max_index {
            return Err(format!(
                "Index {} exceeds max {}",
                index, self.max_index
            ));
        }
        self.current_index = index;
        Ok(())
    }

    fn index(&self) -> usize {
        self.current_index
    }

    fn find_indices(&self, name: &str, cond: FindCondition) -> Result<Vec<usize>, String> {
        let sig = self.file.signal_by_name(name)
            .ok_or_else(|| format!("Unknown signal: {}", name))?;

        let changes = self.signal_data.get(&sig.handle)
            .ok_or_else(|| format!("No data for signal: {}", name))?;

        let mut indices = Vec::new();
        let mut prev_value: Option<u8> = None;

        for (i, &timestamp) in self.timestamps.iter().enumerate() {
            let mut curr_value: Option<u8> = None;

            for &(t, ref val) in changes {
                if t <= timestamp {
                    if val.len() == 1 {
                        curr_value = Some(val[0]);
                    }
                } else {
                    break;
                }
            }

            let matches = match (&cond, prev_value, curr_value) {
                (FindCondition::Rising, Some(0), Some(1)) => true,
                (FindCondition::Falling, Some(1), Some(0)) => true,
                (FindCondition::High, _, Some(1)) => true,
                (FindCondition::Low, _, Some(0)) => true,
                (FindCondition::Value(v), _, Some(val)) => val == *v,
                _ => false,
            };

            if matches {
                indices.push(i);
            }

            prev_value = curr_value;
        }

        Ok(indices)
    }
}