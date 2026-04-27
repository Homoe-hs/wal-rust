//! VCD trace implementation (two-pass scan + sparse index + LRU cache)
//!
//! Optimized loading: zero-copy byte-parsing, packed signal IDs, pre-allocation.

use crate::trace::{Trace, TraceId, ScalarValue, FindCondition};
use crate::vcd::types::VcdValue;
use std::cell::RefCell;
use std::collections::{HashMap, BTreeMap};
use std::path::Path;

/// LRU cache capacity (number of entries)
const DEFAULT_CACHE_CAPACITY: usize = 100_000;

/// Hash a signal ID from raw bytes (e.g. "!" → 0x21, "ab" → 0x6162)
#[inline(always)]
fn hash_sig_id(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0;
    for &b in bytes {
        h = (h << 8) | (b as u64);
    }
    h
}

pub struct VcdTrace {
    id: TraceId,
    filename: String,
    signals: Vec<String>,
    signal_ids: HashMap<u64, u32>,       // hash → index
    signal_widths: HashMap<u32, usize>,
    name_to_idx: HashMap<String, u32>,

    // Pass 1: sparse index
    timestamps: Vec<u64>,
    timestamp_offsets: Vec<u64>,
    sparse_index: HashMap<u32, BTreeMap<u64, u64>>,

    // Pass 2: LRU cache
    lru_cache: RefCell<lru::LruCache<(u32, u64), VcdValue>>,

    // Persistent mmap
    reader: RefCell<crate::vcd::reader::MmapReader>,
    header_end_offset: u64,

    // Runtime
    current_index: usize,
    max_index: usize,
}

impl VcdTrace {
    pub fn load(path: &Path, id: TraceId) -> Result<Self, String> {
        use rayon::prelude::*;
        use std::sync::Arc;

        let filename = path.to_string_lossy().to_string();
        let mut reader = crate::vcd::reader::MmapReader::new(path)
            .map_err(|e| format!("Failed to mmap {}: {}", filename, e))?;

        let file_len = reader.data_len();
        let est_ts = (file_len / 200).max(1024) as usize;

        // ====== PASS 1a: Scan header (single-thread) ======
        let mut signals = Vec::with_capacity(256);
        let mut signal_ids: HashMap<u64, u32> = HashMap::with_capacity(256);
        let mut signal_widths: HashMap<u32, usize> = HashMap::with_capacity(256);
        let mut name_to_idx: HashMap<String, u32> = HashMap::with_capacity(256);
        let mut header_end_offset: u64 = 0;

        loop {
            let line_offset = reader.current_offset();
            let line = match reader.read_line_bytes() {
                Some(l) => l, None => break,
            };
            if line.is_empty() { continue; }
            if line[0] == b'$' {
                if line.len() > 4 && line[1] == b'v' && line[2] == b'a' && line[3] == b'r' {
                    if let Some((sig_hash, name, width)) = parse_var_decl_fast(line) {
                        let idx = signals.len() as u32;
                        signals.push(name.clone());
                        signal_ids.insert(sig_hash, idx);
                        name_to_idx.insert(name, idx);
                        signal_widths.insert(idx, width);
                    }
                } else if line.starts_with(b"$enddefinitions") || line.starts_with(b"$end") {
                    header_end_offset = line_offset + line.len() as u64 + 1;
                    break; // header done
                }
            }
        }

        // If no $enddefinitions found, use current position
        if header_end_offset == 0 {
            header_end_offset = reader.current_offset();
        }

        // ====== PASS 1b: Parallel chunk scan of dump section ======
        let data = reader.data();
        let dump_start = header_end_offset as usize;
        let dump_len = data.len() - dump_start;

        // Skip $dumpvars section to first timestamp
        let mut pos = dump_start;
        while pos < data.len() && data[pos] != b'#' {
            pos += 1;
        }
        let actual_start = pos;

        let n_threads = num_cpus::get();
        let chunk_size = dump_len / n_threads;

        // Find chunk boundaries at newlines
        let mut boundaries = vec![actual_start];
        for i in 1..n_threads {
            let mut p = actual_start + i * chunk_size;
            while p < data.len() && data[p] != b'\n' {
                p += 1;
            }
            if p < data.len() { p += 1; } // skip newline
            boundaries.push(p);
        }
        boundaries.push(data.len());

        // Shared read-only signal_ids for all threads
        let signal_ids_arc = Arc::new(signal_ids);

        // Parallel processing
        let sparse_interval: u64 = 100;
        let results: Vec<(Vec<u64>, Vec<u64>, HashMap<u32, BTreeMap<u64, u64>>)> = boundaries
            .par_windows(2)
            .map(|w| {
                let chunk_start = w[0];
                let chunk_end = w[1];
                let chunk = &data[chunk_start..chunk_end];
                let sid = signal_ids_arc.clone(); // shared ref

                let mut ts = Vec::new();
                let mut ts_offsets = Vec::new();
                let mut si: HashMap<u32, BTreeMap<u64, u64>> = HashMap::new();
                let mut change_counts: HashMap<u32, u64> = HashMap::new();
                let mut current_timestamp: u64 = 0;
                let base_offset = chunk_start as u64;

                let mut lp = 0usize;
                while lp < chunk.len() {
                    // Find next newline
                    let line_start = lp;
                    while lp < chunk.len() && chunk[lp] != b'\n' {
                        lp += 1;
                    }
                    let line_end = lp;
                    if lp < chunk.len() { lp += 1; } // skip newline

                    let line = &chunk[line_start..line_end];
                    if line.is_empty() { continue; }

                    let first = line[0];
                    match first {
                        b'#' => {
                            current_timestamp = parse_timestamp_fast(line);
                            ts.push(current_timestamp);
                            ts_offsets.push(base_offset + line_start as u64);
                        }
                        b'$' => {}
                        _ => {
                            if let Some((sig_hash, _value)) = parse_value_change_fast(line) {
                                if let Some(&sig_idx) = sid.get(&sig_hash) {
                                    let count = change_counts.entry(sig_idx).or_insert(0);
                                    *count += 1;
                                    if *count % sparse_interval == 0 {
                                        si.entry(sig_idx)
                                            .or_default()
                                            .insert(current_timestamp, base_offset + line_start as u64);
                                    }
                                }
                            }
                        }
                    }
                }
                (ts, ts_offsets, si)
            })
            .collect();

        // ====== MERGE RESULTS ======
        let mut timestamps: Vec<u64> = Vec::with_capacity(est_ts);
        let mut timestamp_offsets: Vec<u64> = Vec::with_capacity(est_ts);
        let mut sparse_index: HashMap<u32, BTreeMap<u64, u64>> = HashMap::with_capacity(128);
        // Init empty btreemaps for each signal
        for idx in 0..signals.len() {
            sparse_index.insert(idx as u32, BTreeMap::new());
        }

        for (chunk_ts, chunk_offsets, chunk_si) in results {
            timestamps.extend(chunk_ts);
            timestamp_offsets.extend(chunk_offsets);
            for (sig_idx, entries) in chunk_si {
                sparse_index.entry(sig_idx).or_default().extend(entries);
            }
        }

        let signal_ids = Arc::try_unwrap(signal_ids_arc).unwrap_or_else(|arc| (*arc).clone());
        let max_index = if timestamps.is_empty() { 0 } else { timestamps.len() - 1 };

        reader.seek_to(0).map_err(|e| format!("Seek error: {}", e))?;

        Ok(VcdTrace {
            id, filename,
            signals, signal_ids, signal_widths, name_to_idx,
            timestamps, timestamp_offsets, sparse_index,
            lru_cache: RefCell::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(DEFAULT_CACHE_CAPACITY).unwrap(),
            )),
            reader: RefCell::new(reader),
            header_end_offset,
            current_index: 0, max_index,
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

    /// On-demand: read signal value at a specific timestamp (optimized with u64 hash)
    fn read_signal_value_at(&self, sig_idx: u32, target_timestamp: u64) -> VcdValue {
        let mut reader = self.reader.borrow_mut();

        let start_offset = self
            .sparse_index
            .get(&sig_idx)
            .and_then(|idx_map| idx_map.range(..=target_timestamp).last())
            .map(|(_, &off)| off)
            .unwrap_or(self.header_end_offset);

        let _ = reader.seek_to(start_offset);
        let mut last_value: Option<VcdValue> = None;

        loop {
            let line = match reader.read_line_bytes() {
                Some(l) => l,
                None => break,
            };
            if line.is_empty() { continue; }
            let first = line[0];

            if first == b'#' {
                let ts = parse_timestamp_fast(line);
                if ts > target_timestamp { break; }
            } else if first != b'$' {
                if let Some((sig_hash, value)) = parse_value_change_fast(line) {
                    // Check if this is our signal (need to verify hash)
                    if self.signal_ids.get(&sig_hash) == Some(&sig_idx) {
                        last_value = Some(value);
                    }
                }
            }
        }
        last_value.unwrap_or(VcdValue::Bit(b'x'))
    }

    /// On-demand find bit value at timestamp
    fn read_bit_value_at(&self, sig_idx: u32, target_timestamp: u64) -> Option<u8> {
        let mut reader = self.reader.borrow_mut();

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
            if line.is_empty() { continue; }
            let first = line[0];

            if first == b'#' {
                let ts = parse_timestamp_fast(line);
                if ts > target_timestamp { break; }
            } else if first != b'$' {
                if let Some((sig_hash, value)) = parse_value_change_fast(line) {
                    if self.signal_ids.get(&sig_hash) == Some(&sig_idx) {
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

// ================ FAST BYTE-LEVEL PARSE FUNCTIONS ================

/// Parse timestamp: "#12345" → 12345
#[inline(always)]
fn parse_timestamp_fast(line: &[u8]) -> u64 {
    let mut n: u64 = 0;
    for &b in &line[1..] {
        if b < b'0' || b > b'9' { break; }
        n = n * 10 + (b - b'0') as u64;
    }
    n
}

/// Parse a $var declaration: extract (sig_hash, name, width)
/// Format: $var wire 32 ! signal_name $end
fn parse_var_decl_fast(line: &[u8]) -> Option<(u64, String, usize)> {
    // Find width (3rd field), signal ID (4th field), name (5th field to $end)
    let mut parts = [0usize; 6]; // start offsets of fields
    let mut part = 0;
    let mut in_field = false;

    for (i, &b) in line.iter().enumerate() {
        if b == b' ' || b == b'\t' {
            if in_field { part += 1; in_field = false; }
        } else if !in_field {
            if part < 6 { parts[part] = i; }
            in_field = true;
        }
        if part >= 6 { break; }
    }
    if part < 5 { return None; }

    // parts[1] = type, parts[2] = width, parts[3] = ID, parts[4] = name start
    let width: usize = {
        let mut w: usize = 0;
        for &b in &line[parts[2]..] {
            if b < b'0' || b > b'9' { break; }
            w = w * 10 + (b - b'0') as usize;
        }
        w
    };
    if width == 0 { return None; }

    // Signal ID: bytes from parts[3] to next space
    let id_start = parts[3];
    let id_end = line[id_start..].iter().position(|&b| b == b' ').map(|p| id_start + p).unwrap_or(line.len());
    let sig_hash = hash_sig_id(&line[id_start..id_end]);

    // Name: from parts[4] to $end
    let name_start = parts[4];
    let name_end = line[name_start..].iter().position(|&b| b == b' ' || b == b'$').map(|p| name_start + p).unwrap_or(line.len());
    let name = std::str::from_utf8(&line[name_start..name_end]).ok()?.to_string();

    Some((sig_hash, name, width))
}

/// Parse value change: "0!" or "b1010 !" or "r3.14 !" → (sig_hash, value)
/// Ultra-fast byte parsing — no from_utf8(), no memchr, no allocations
#[inline(always)]
fn parse_value_change_fast(line: &[u8]) -> Option<(u64, VcdValue)> {
    let len = line.len();
    if len < 2 { return None; }

    // Find last space by scanning backwards — VCD lines are short (2-50 bytes)
    let mut sp = len;
    for i in (0..len).rev() {
        if line[i] == b' ' {
            sp = i;
            break;
        }
    }

    let (value_part, sig_id_bytes) = if sp < len {
        (&line[..sp], &line[sp+1..])
    } else {
        // No space: scalar concat "0!", "1!" etc.
        let first = line[0];
        if matches!(first, b'0' | b'1' | b'x' | b'X' | b'z' | b'Z') && line.len() >= 2 {
            (&line[..1], &line[1..])
        } else {
            return None;
        }
    };

    if sig_id_bytes.is_empty() || sig_id_bytes[0] == b'$' { return None; }

    let sig_hash = hash_sig_id(sig_id_bytes);
    let vfirst = value_part[0];

    let value = match vfirst {
        b'b' => VcdValue::Vector(value_part[1..].to_vec()),
        b'r' => {
            if let Ok(s) = std::str::from_utf8(&value_part[1..]) {
                if let Ok(r) = s.parse::<f64>() {
                    VcdValue::Real(r)
                } else { return None; }
            } else { return None; }
        }
        _ => VcdValue::Bit(vfirst),
    };

    Some((sig_hash, value))
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

