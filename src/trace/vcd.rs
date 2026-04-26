//! VCD trace implementation (two-pass scan + sparse index + LRU cache)

use crate::trace::{Trace, TraceId, ScalarValue, FindCondition};
use crate::vcd::types::VcdValue;
use std::cell::RefCell;
use std::collections::{HashMap, BTreeMap};
use std::path::Path;

/// LRU cache capacity (number of entries)
const DEFAULT_CACHE_CAPACITY: usize = 100_000;

pub struct VcdTrace {
    id: TraceId,
    filename: String,
    signals: Vec<String>,
    signal_ids: HashMap<String, u32>,
    signal_widths: HashMap<u32, usize>,

    // Pass 1: sparse index (low memory)
    timestamps: Vec<u64>,
    timestamp_offsets: Vec<u64>,   // file byte offset for each timestamp line
    sparse_index: HashMap<u32, BTreeMap<u64, u64>>, // sig_idx → (timestamp → file_offset)
    name_to_idx: HashMap<String, u32>,  // signal name → signal index

    // Pass 2: LRU cache
    lru_cache: RefCell<lru::LruCache<(u32, u64), VcdValue>>,

    // Persistent mmap for on-demand queries
    reader: RefCell<crate::vcd::reader::MmapReader>,
    header_end_offset: u64,

    // Runtime state
    current_index: usize,
    max_index: usize,
}

impl VcdTrace {
    pub fn load(path: &Path, id: TraceId) -> Result<Self, String> {
        let filename = path.to_string_lossy().to_string();

        let mut reader = crate::vcd::reader::MmapReader::new(path)
            .map_err(|e| format!("Failed to mmap {}: {}", filename, e))?;

        let mut signals = Vec::new();
        let mut signal_ids = HashMap::new();
        let mut signal_widths = HashMap::new();
        let mut name_to_idx = HashMap::new();
        let mut timestamps = Vec::new();
        let mut timestamp_offsets = Vec::new();
        let mut sparse_index: HashMap<u32, BTreeMap<u64, u64>> = HashMap::new();

        // Track value change counts per signal for sparse sampling
        let mut change_counts: HashMap<u32, u64> = HashMap::new();
        let sparse_interval: u64 = 100; // sample every N changes

        let mut header_end_offset: u64 = 0;
        let mut current_timestamp: u64 = 0;
        let mut in_header = true;

        loop {
            let line_offset = reader.current_offset();
            let line = match reader.read_line_bytes() {
                Some(l) => l,
                None => break,
            };

            if line.is_empty() {
                continue;
            }

            if in_header {
                if line.starts_with(b"$var") {
                    if let Some(result) = parse_var_decl(line) {
                        let (sig_id, name, width) = result;
                        let idx = signals.len() as u32;
                        signals.push(name.clone());
                        signal_ids.insert(sig_id.clone(), idx);
                        name_to_idx.insert(name, idx);
                        signal_widths.insert(idx, width);
                        sparse_index.insert(idx, BTreeMap::new());
                        change_counts.insert(idx, 0);
                    }
                } else if line.starts_with(b"$enddefinitions") {
                    in_header = false;
                    header_end_offset = line_offset + line.len() as u64 + 1;
                }
                continue;
            }

            // Dump section
            if line.starts_with(b"#") {
                // Timestamp line: #12345
                if let Some(ts) = parse_timestamp(line) {
                    current_timestamp = ts;
                    timestamps.push(ts);
                    timestamp_offsets.push(line_offset);
                }
            } else if !line.starts_with(b"$") {
                // Value change line: 'b0' or '1' etc followed by signal ID
                if let Some((sig_id, _value)) = parse_value_change(line) {
                    if let Some(&sig_idx) = signal_ids.get(&sig_id) {
                        let count = change_counts.entry(sig_idx).or_insert(0);
                        *count += 1;
                        if *count % sparse_interval == 0 {
                            sparse_index
                                .entry(sig_idx)
                                .or_default()
                                .insert(current_timestamp, line_offset);
                        }
                    }
                }
            }
        }

        let max_index = if timestamps.is_empty() { 0 } else { timestamps.len() - 1 };

        // Reset reader position for on-demand queries
        reader.seek_to(0).map_err(|e| format!("Seek error: {}", e))?;

        Ok(VcdTrace {
            id,
            filename,
            signals,
            signal_ids,
            signal_widths,
            timestamps,
            timestamp_offsets,
            sparse_index,
            name_to_idx,
            lru_cache: RefCell::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(DEFAULT_CACHE_CAPACITY)
                    .expect("cache capacity must be non-zero"),
            )),
            reader: RefCell::new(reader),
            header_end_offset,
            current_index: 0,
            max_index,
        })
    }

    /// Find the index of a given timestamp (returns nearest <= target)
    #[allow(dead_code)]
    fn find_timestamp_index(&self, target: u64) -> usize {
        match self.timestamps.binary_search(&target) {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) => i - 1,
        }
    }

    /// On-demand: read signal value at a specific timestamp offset
    fn read_signal_value_at(&self, sig_idx: u32, target_timestamp: u64) -> VcdValue {
        let mut reader = self.reader.borrow_mut();

        let sig_id = self
            .signal_ids
            .iter()
            .find_map(|(id, &idx)| if idx == sig_idx { Some(id.clone()) } else { None })
            .unwrap_or_default();

        // Find the nearest sparse checkpoint BEFORE target
        let start_offset = self
            .sparse_index
            .get(&sig_idx)
            .and_then(|idx_map| idx_map.range(..=target_timestamp).last())
            .map(|(_, &off)| off)
            .unwrap_or(self.header_end_offset);

        // Seek to starting position
        let _ = reader.seek_to(start_offset);

        let mut last_value: Option<VcdValue> = None;

        loop {
            let line = match reader.read_line_bytes() {
                Some(l) => l,
                None => break,
            };

            if line.is_empty() {
                continue;
            }

            if line.starts_with(b"#") {
                if let Some(ts) = parse_timestamp(line) {
                    if ts > target_timestamp {
                        break;
                    }
                }
            } else if !line.starts_with(b"$") {
                if let Some((change_id, value)) = parse_value_change(line) {
                    if change_id == sig_id {
                        last_value = Some(value);
                    }
                }
            }
        }

        last_value.unwrap_or(VcdValue::Bit(b'x'))
    }

    /// On-demand find value at timestamp (used by find_indices)
    fn read_bit_value_at(&self, sig_idx: u32, target_timestamp: u64) -> Option<u8> {
        let mut reader = self.reader.borrow_mut();

        let sig_id = self
            .signal_ids
            .iter()
            .find_map(|(id, &idx)| if idx == sig_idx { Some(id.clone()) } else { None })
            .unwrap_or_default();

        let start_offset = self
            .sparse_index
            .get(&sig_idx)
            .and_then(|idx_map| idx_map.range(..=target_timestamp).last())
            .map(|(_, &off)| off)
            .unwrap_or(self.header_end_offset);

        let _ = reader.seek_to(start_offset);

        let mut last_value: Option<u8> = None;

        loop {
            let line = match reader.read_line_bytes() {
                Some(l) => l,
                None => break,
            };

            if line.is_empty() {
                continue;
            }

            if line.starts_with(b"#") {
                if let Some(ts) = parse_timestamp(line) {
                    if ts > target_timestamp {
                        break;
                    }
                }
            } else if !line.starts_with(b"$") {
                if let Some((change_id, value)) = parse_value_change(line) {
                    if change_id == sig_id {
                        last_value = match value {
                            VcdValue::Bit(b) => Some(b),
                            _ => None,
                        };
                    }
                }
            }
        }

        last_value
    }
}

/// Parse a $var line: "$var wire 8 n0 signal_name $end"
/// Returns (signal_id, signal_name, width)
fn parse_var_decl(line: &[u8]) -> Option<(String, String, usize)> {
    // $var type width id name $end
    let line_str = std::str::from_utf8(line).ok()?;
    let parts: Vec<&str> = line_str.split_whitespace().collect();
    if parts.len() < 6 {
        return None;
    }
    let var_type = parts[1];
    let width: usize = parts[2].parse().ok()?;
    let sig_id = parts[3].to_string();

    // Name is everything between id and $end
    let name_start = line_str.find(sig_id.as_str())? + sig_id.len() + 1;
    let name_end = line_str.rfind("$end").unwrap_or(line_str.len());
    let name = line_str[name_start..name_end].trim().to_string();

    let full_name = if var_type == "wire" || var_type == "reg" || var_type == "real" {
        name
    } else {
        format!("{} {}", var_type, name)
    };

    Some((sig_id, full_name, width))
}

/// Parse a timestamp line: "#12345"
fn parse_timestamp(line: &[u8]) -> Option<u64> {
    if line.len() < 2 || line[0] != b'#' {
        return None;
    }
    let ts_str = std::str::from_utf8(&line[1..]).ok()?;
    ts_str.parse().ok()
}

/// Parse a value change line: "b0 n0" or "1n0" or "r3.14 n0"
/// Returns (signal_id, value)
fn parse_value_change(line: &[u8]) -> Option<(String, VcdValue)> {
    let content = std::str::from_utf8(line).ok()?;
    let content = content.trim();

    if content.is_empty() {
        return None;
    }

    // Find the signal ID part (last non-whitespace segment)
    let sig_id = content.split_whitespace().last()?;
    if sig_id.is_empty() || sig_id.starts_with('$') {
        return None;
    }

    let value_part = &content[..content.len() - sig_id.len()].trim();

    // Handle scalar concatenation: "0!", "1.", "x#", "z$" (no space)
    if value_part.is_empty() && sig_id.len() >= 2 {
        let ch = sig_id.as_bytes()[0];
        if matches!(ch, b'0' | b'1' | b'x' | b'X' | b'z' | b'Z') {
            let real_sig_id = &sig_id[1..];
            return Some((real_sig_id.to_string(), VcdValue::Bit(ch)));
        }
    }

    if let Some(vector_part) = value_part.strip_prefix('b') {
        let vector: Vec<u8> = vector_part.bytes().collect();
        Some((sig_id.to_string(), VcdValue::Vector(vector)))
    } else if let Some(real_part) = value_part.strip_prefix('r') {
        let real: f64 = real_part.parse().ok()?;
        Some((sig_id.to_string(), VcdValue::Real(real)))
    } else if value_part.len() == 1 {
        let ch = value_part.as_bytes()[0];
        Some((sig_id.to_string(), VcdValue::Bit(ch)))
    } else {
        None
    }
}

impl Trace for VcdTrace {
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
        self.signals.clear();
        self.signal_ids.clear();
        self.signal_widths.clear();
        self.timestamps.clear();
        self.timestamp_offsets.clear();
        self.sparse_index.clear();
        self.lru_cache.borrow_mut().clear();
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
        let idx = if offset < self.timestamps.len() {
            offset
        } else {
            return Err(format!("Offset {} out of range", offset));
        };

        let target_time = self.timestamps[idx];
        let sig_idx = self.name_to_idx.get(name).copied()
            .ok_or_else(|| format!("Unknown signal: {}", name))?;

        // Check LRU cache
        let cache_key = (sig_idx, target_time);
        if let Some(cached) = self.lru_cache.borrow_mut().get(&cache_key).cloned() {
            return Ok(value_to_scalar(&cached));
        }

        // On-demand read from mmap
        let val = self.read_signal_value_at(sig_idx, target_time);
        self.lru_cache.borrow_mut().put(cache_key, val.clone());
        Ok(value_to_scalar(&val))
    }

    fn signal_width(&self, name: &str) -> Result<usize, String> {
        let sig_idx = self.name_to_idx.get(name).copied()
            .ok_or_else(|| format!("Unknown signal: {}", name))?;
        Ok(self.signal_widths.get(&sig_idx).copied().unwrap_or(1))
    }

    fn signals(&self) -> Vec<String> {
        self.signals.clone()
    }

    fn scopes(&self) -> Vec<String> {
        Vec::new()
    }

    fn max_index(&self) -> usize {
        self.max_index
    }

    fn set_index(&mut self, index: usize) -> Result<(), String> {
        if index > self.max_index {
            return Err(format!("Index {} exceeds max {}", index, self.max_index));
        }
        self.current_index = index;
        Ok(())
    }

    fn index(&self) -> usize {
        self.current_index
    }

    fn find_indices(&self, name: &str, cond: FindCondition) -> Result<Vec<usize>, String> {
        let sig_idx = self.name_to_idx.get(name).copied()
            .ok_or_else(|| format!("Unknown signal: {}", name))?;

        let mut indices = Vec::new();
        let mut prev_value: Option<u8> = None;

        for (i, &target_time) in self.timestamps.iter().enumerate() {
            let curr_value = self.read_bit_value_at(sig_idx, target_time);

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

fn value_to_scalar(val: &VcdValue) -> ScalarValue {
    match val {
        VcdValue::Bit(b) => ScalarValue::Bit(*b),
        VcdValue::Vector(v) => ScalarValue::Vector(v.clone()),
        VcdValue::Real(r) => ScalarValue::Real(*r),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_value_change_scalar_no_space() {
        // Test the fix for scalar values without space separator
        assert_eq!(
            parse_value_change(b"0\""),
            Some(("\"".to_string(), VcdValue::Bit(b'0')))
        );
        assert_eq!(
            parse_value_change(b"1\""),
            Some(("\"".to_string(), VcdValue::Bit(b'1')))
        );
        assert_eq!(
            parse_value_change(b"x!"),
            Some(("!".to_string(), VcdValue::Bit(b'x')))
        );
        assert_eq!(
            parse_value_change(b"0."),
            Some((".".to_string(), VcdValue::Bit(b'0')))
        );
    }

    #[test]
    fn test_parse_value_change_vector_with_space() {
        assert_eq!(
            parse_value_change(b"b1010 !"),
            Some(("!".to_string(), VcdValue::Vector(b"1010".to_vec())))
        );
    }

    #[test]
    fn test_parse_value_change_vector_no_space() {
        // Vector without space should NOT be parsed as scalar
        assert_eq!(
            parse_value_change(b"b1010!"),
            None
        );
    }
}
