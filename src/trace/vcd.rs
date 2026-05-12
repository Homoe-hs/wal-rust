//! VCD trace implementation (two-pass scan + sparse index + LRU cache)
//!
//! Optimized loading: zero-copy byte-parsing, packed signal IDs, pre-allocation.

use crate::trace::{Trace, TraceId, ScalarValue, FindCondition};
use crate::vcd::types::VcdValue;
use std::cell::RefCell;
use std::collections::{HashMap, BTreeMap, HashSet};
use std::path::Path;

/// LRU cache capacity (number of entries)
fn adapt_lru_capacity(file_size: usize, signal_count: usize) -> usize {
    // Base: 5 timestamps per signal, capped at 200K signals
    let base = (signal_count as u64).min(200_000) * 5;
    // File size factor: larger files need larger cache
    let file_factor = match file_size {
        0..=1_000_000_000 => 1,        // <1GB
        1_000_000_001..=50_000_000_000 => 3,   // 1-50GB
        _ => 5,                          // >50GB
    };
    (base * file_factor).max(50_000).min(5_000_000) as usize
}

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
    #[allow(dead_code)]
    signal_ids: HashMap<u64, u32>,       // hash → index
    signal_id_bytes: HashMap<u32, Vec<u8>>, // index → VCD signal ID bytes (for fast matching)
    signal_widths: HashMap<u32, usize>,
    name_to_idx: HashMap<String, u32>,

    // Event signals (VCD event type — auto-reset to 0 at each timestamp boundary)
    event_signals: HashSet<u32>,
    // Pre-recorded change point INDICES for event signals (built during PASS 1b)
    event_change_points: HashMap<u32, Vec<usize>>,

    // Pass 1: sparse index
    timestamps: Vec<u64>,
    #[allow(dead_code)]
    timestamp_offsets: Vec<u64>,
    sparse_index: HashMap<u32, BTreeMap<u64, u64>>,

    // Pass 2: LRU cache
    lru_cache: RefCell<lru::LruCache<(u32, u64), VcdValue>>,

    // Persistent mmap
    reader: RefCell<crate::vcd::reader::MmapReader>,
    header_end_offset: u64,

    // Scopes
    scopes: Vec<String>,

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
        let mut signal_id_bytes: HashMap<u32, Vec<u8>> = HashMap::with_capacity(256);
        let mut signal_widths: HashMap<u32, usize> = HashMap::with_capacity(256);
        let mut name_to_idx: HashMap<String, u32> = HashMap::with_capacity(256);
        let mut event_signals: HashSet<u32> = HashSet::new();
        let mut header_end_offset: u64 = 0;

        // Track $scope / $upscope for hierarchical signal names
        let mut scope_stack: Vec<String> = Vec::new();
        let mut scopes: Vec<String> = Vec::new();

        loop {
            let line_offset = reader.current_offset();
            let line = match reader.read_line_bytes() {
                Some(l) => l, None => break,
            };
            if line.is_empty() { continue; }
            if line[0] == b'$' {
                if line.len() > 4 && line[1] == b'v' && line[2] == b'a' && line[3] == b'r' {
                    if let Some((sig_hash, short_name, width, id_bytes, is_event)) = parse_var_decl_fast2(line) {
                        let idx = signals.len() as u32;
                        // Build full hierarchical name from scope stack
                        let full_name = if scope_stack.is_empty() {
                            short_name.clone()
                        } else {
                            format!("{}.{}", scope_stack.join("."), short_name)
                        };
                        signals.push(full_name.clone());
                        signal_ids.insert(sig_hash, idx);
                        signal_id_bytes.insert(idx, id_bytes);
                        name_to_idx.insert(full_name, idx);
                        signal_widths.insert(idx, width);
                        if is_event { event_signals.insert(idx); }
                    }
                } else if line.starts_with(b"$scope") {
                    // $scope module name $end → push scope name
                    if let Some(scope_name) = parse_scope_name(line) {
                        scope_stack.push(scope_name);
                        scopes.push(scope_stack.join("."));
                    }
                } else if line.starts_with(b"$upscope") {
                    scope_stack.pop();
                } else if line.starts_with(b"$enddefinitions") {
                    header_end_offset = line_offset + line.len() as u64 + 1;
                    break;
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
            if p >= data.len() {
                break; // no more chunks needed
            }
            while p < data.len() && data[p] != b'\n' {
                p += 1;
            }
            if p < data.len() { p += 1; } // skip newline
            if p >= data.len() { break; } // don't push past the end
            boundaries.push(p);
        }
        boundaries.push(data.len());

        // Shared read-only data for parallel threads
        let signal_ids_arc = Arc::new(signal_ids);
        let has_events = !event_signals.is_empty();
        let event_sigs_arc = Arc::new(event_signals.clone());

        // Parallel processing
        let sparse_interval: u64 = 100;
        let results: Vec<(Vec<u64>, Vec<u64>, HashMap<u32, BTreeMap<u64, u64>>, Vec<(u32, u64)>)> = boundaries
            .par_windows(2)
            .map(|w| {
                let chunk_start = w[0];
                let chunk_end = w[1];
                let chunk = &data[chunk_start..chunk_end];
                let sid = signal_ids_arc.clone();
                let evt = event_sigs_arc.clone();

                let mut ts = Vec::new();
                let mut ts_offsets = Vec::new();
                let mut si: HashMap<u32, BTreeMap<u64, u64>> = HashMap::new();
                let mut change_counts: HashMap<u32, u64> = HashMap::new();
                let mut event_cp: Vec<(u32, u64)> = Vec::new();
                let mut current_timestamp: u64 = 0;
                let base_offset = chunk_start as u64;

                let mut lp = 0usize;
                while lp < chunk.len() {
                    let line_start = lp;
                    let line_end = match memchr::memchr(b'\n', &chunk[lp..]) {
                        Some(nl_pos) => {
                            lp += nl_pos;
                            let end = lp;
                            lp += 1; // skip newline
                            end
                        }
                    None => { break; }
                    };
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
                                    if has_events && evt.contains(&sig_idx) {
                                        event_cp.push((sig_idx, current_timestamp));
                                    }
                                }
                            }
                        }
                    }
                }
                (ts, ts_offsets, si, event_cp)
            })
            .collect();

        // ====== MERGE RESULTS ======
        let mut timestamps: Vec<u64> = Vec::with_capacity(est_ts);
        let mut timestamp_offsets: Vec<u64> = Vec::with_capacity(est_ts);
        let mut sparse_index: HashMap<u32, BTreeMap<u64, u64>> = HashMap::with_capacity(128);
        let mut event_change_ts: HashMap<u32, Vec<u64>> = HashMap::new();
        // Init empty btreemaps for each signal
        for idx in 0..signals.len() {
            sparse_index.insert(idx as u32, BTreeMap::new());
        }

        for (chunk_ts, chunk_offsets, chunk_si, chunk_evt) in results {
            timestamps.extend(chunk_ts);
            timestamp_offsets.extend(chunk_offsets);
            for (sig_idx, entries) in chunk_si {
                sparse_index.entry(sig_idx).or_default().extend(entries);
            }
            // Group flat event tuples by sig_idx
            for (sig_idx, ts) in chunk_evt {
                event_change_ts.entry(sig_idx).or_default().push(ts);
            }
        }

        // Convert event timestamps to sequential INDEX using sorted timestamps
        let mut event_change_points: HashMap<u32, Vec<usize>> = HashMap::with_capacity(event_change_ts.len());
        for (sig_idx, ts_list) in &event_change_ts {
            let mut indices = Vec::with_capacity(ts_list.len());
            let mut ti = 0usize;
            for ts in ts_list {
                while ti < timestamps.len() && timestamps[ti] < *ts { ti += 1; }
                if ti < timestamps.len() && timestamps[ti] == *ts {
                    indices.push(ti);
                }
            }
            event_change_points.insert(*sig_idx, indices);
        }

        let signal_ids = Arc::try_unwrap(signal_ids_arc).unwrap_or_else(|arc| (*arc).clone());
        let max_index = if timestamps.is_empty() { 0 } else { timestamps.len() - 1 };
        let lru_cap = std::num::NonZeroUsize::new(adapt_lru_capacity(file_len, signals.len() as usize))
            .unwrap_or(std::num::NonZeroUsize::MIN);

        reader.seek_to(0).map_err(|e| format!("Seek error: {}", e))?;

        Ok(VcdTrace {
            id, filename,
            signals, signal_ids, signal_id_bytes, signal_widths, name_to_idx, event_signals, event_change_points,
            timestamps, timestamp_offsets, sparse_index,
            lru_cache: RefCell::new(lru::LruCache::new(lru_cap)),
            reader: RefCell::new(reader),
            header_end_offset,
            scopes,
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

    /// On-demand: read signal value at a specific timestamp (optimized with direct byte match)
    fn read_signal_value_at(&self, sig_idx: u32, target_timestamp: u64) -> VcdValue {
        let target_id = match self.signal_id_bytes.get(&sig_idx) {
            Some(id) => id,
            None => return VcdValue::Bit(b'x'),
        };
        let id_len = target_id.len();
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
                if self.event_signals.contains(&sig_idx) {
                    last_value = Some(VcdValue::Bit(b'0'));
                }
            } else if first != b'$' && line.len() > id_len {
                let id_start = line.len() - id_len;
                if &line[id_start..] == target_id.as_slice() {
                    let val = match first {
                        b'b' => {
                            let ve = id_start.saturating_sub(1);
                            let vs = if ve > 1 && line[ve] == b' ' { &line[1..ve] } else { &line[1..id_start] };
                            VcdValue::Vector(vs.to_vec())
                        }
                        b'r' => {
                            let vs = std::str::from_utf8(&line[1..id_start]).unwrap_or("0");
                            if let Ok(r) = vs.trim().parse::<f64>() { VcdValue::Real(r) } else { continue; }
                        }
                        _ => VcdValue::Bit(first),
                    };
                    last_value = Some(val);
                }
            }
        }
        last_value.unwrap_or(VcdValue::Bit(b'x'))
    }

    /// On-demand find bit value at timestamp
    #[allow(dead_code)]
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
                if self.event_signals.contains(&sig_idx) {
                    last_value = Some(b'0');
                }
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

/// Parse a $scope line: "$scope module name $end" → "name"
fn parse_scope_name(line: &[u8]) -> Option<String> {
    let line_str = std::str::from_utf8(line).ok()?;
    let parts: Vec<&str> = line_str.split_whitespace().collect();
    if parts.len() < 3 { return None; }
    let name = if parts.len() >= 4 {
        parts[2..].join(" ")
    } else {
        parts[2].to_string()
    };
    let name = name.trim_end_matches("$end").trim().to_string();
    if name.is_empty() { return None; }
    Some(name)
}

/// Parse a $var declaration: extract (sig_hash, name, width)
/// Format: $var wire 32 ! signal_name $end
#[allow(dead_code)]
fn parse_var_decl_fast(line: &[u8]) -> Option<(u64, String, usize)> {
    let (hash, name, width, _, _) = parse_var_decl_fast2(line)?;
    Some((hash, name, width))
}

/// Parse a $var declaration, also returning raw signal ID bytes and is_event flag
fn parse_var_decl_fast2(line: &[u8]) -> Option<(u64, String, usize, Vec<u8>, bool)> {
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
    if part < 5 {
        return None;
    }

    // Detect type (2nd field, e.g. "wire", "reg", "event")
    let type_str = std::str::from_utf8(&line[parts[1]..parts[2]]).unwrap_or("wire");
    let is_event = type_str.trim() == "event";

    let width: usize = {
        let mut w: usize = 0;
        for &b in &line[parts[2]..] {
            if b < b'0' || b > b'9' { break; }
            w = w * 10 + (b - b'0') as usize;
        }
        w
    };
    if width == 0 {
        return None;
    }

    // Signal ID: bytes from parts[3] to next space
    let id_start = parts[3];
    let id_end = line[id_start..].iter().position(|&b| b == b' ').map(|p| id_start + p).unwrap_or(line.len());
    let sig_hash = hash_sig_id(&line[id_start..id_end]);
    let id_bytes = line[id_start..id_end].to_vec();

    // Name: from parts[4] to $end (spaces are valid in signal names)
    let name_start = parts[4];
    let name_end = line[name_start..].iter().position(|&b| b == b'$').map(|p| name_start + p).unwrap_or(line.len());
    let name_str = std::str::from_utf8(&line[name_start..name_end]).ok()?;
    let name = name_str.trim().to_string();

    Some((sig_hash, name, width, id_bytes, is_event))
}

/// Parse value change: "0!" or "b1010 !" or "r3.14 !" → (sig_hash, value)
/// Also handles no-space formats: "0!", "1!", "b1010!"
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
        // Space found: "b1010 !" or "0 !"
        (&line[..sp], &line[sp+1..])
    } else {
        // No space: "0!" (scalar) or "b1010!" (vector no-space)
        let first = line[0];
        if matches!(first, b'0' | b'1' | b'x' | b'X' | b'z' | b'Z') && line.len() >= 2 {
            (&line[..1], &line[1..])
        } else if first == b'b' && line.len() >= 3 {
            // No-space vector: "b1010!" → find boundary between binary digits and signal ID
            // Binary digits are '0','1','x','z'; signal ID starts with printable ASCII
            let split = 1 + line[1..].iter()
                .position(|&b| !matches!(b, b'0' | b'1' | b'x' | b'z' | b'X' | b'Z'))
                .unwrap_or(line.len().saturating_sub(1));
            if split > 1 && split < line.len() {
                (&line[..split], &line[split..])
            } else {
                return None;
            }
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
            return Err(format!(
                "signal_value: offset {} out of range (max {}) for signal '{}'",
                offset, self.timestamps.len().max(1u64 as usize)-1, name
            ));
        };

        let target_time = self.timestamps[idx];
        let sig_idx = self.name_to_idx.get(name).copied()
            .ok_or_else(|| format!(
                "signal '{}' not found. Available signals (first 5): {:?}",
                name,
                self.signals.iter().take(5).collect::<Vec<_>>()
            ))?;

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
        self.scopes.clone()
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

        // Event signal fast path: change points already recorded during load
        if self.event_signals.contains(&sig_idx) {
            if let Some(points) = self.event_change_points.get(&sig_idx) {
                if matches!(&cond,
                    FindCondition::Value(1) | FindCondition::ValueI64(1)
                    | FindCondition::Rising | FindCondition::Neq(0)
                    | FindCondition::NeqI64(0) | FindCondition::High)
                {
                    return Ok(points.clone());
                }
                if matches!(&cond, FindCondition::Neq(1) | FindCondition::Low) {
                    // All timestamps except event points
                    let all: Vec<usize> = (0..=self.max_index).collect();
                    let points_set: std::collections::HashSet<usize> = points.iter().copied().collect();
                    return Ok(all.into_iter().filter(|i| !points_set.contains(i)).collect());
                }
            }
            return Ok(vec![]);
        }

        use rayon::prelude::*;

        let target_id = self.signal_id_bytes.get(&sig_idx).cloned()
            .ok_or_else(|| "find_indices: signal ID bytes not found".to_string())?;

        // Get shared mmap via Arc — drop RefCell borrow before rayon parallel section
        let shared_mmap = self.reader.borrow().data.clone();
        let id_len = target_id.len();
        let hdr_end = self.header_end_offset as usize;
        let data_len = shared_mmap.len();

        let n_threads = num_cpus::get().max(4);
        let dump_len = data_len.saturating_sub(hdr_end);
        let chunk_size = dump_len / n_threads.max(1);

        // Build newline-aligned chunk boundaries starting from hdr_end (includes $dumpvars)
        let mut boundaries = vec![hdr_end];
        for i in 1..n_threads {
            let mut p = hdr_end + i * chunk_size;
            if p >= data_len { break; }
            while p < data_len && shared_mmap[p] != b'\n' { p += 1; }
            if p < data_len { p += 1; }
            if p >= data_len { break; }
            boundaries.push(p);
        }
        boundaries.push(data_len);

        // Pre-compute ts_idx at each boundary (one linear scan)
        let boundary_ts: Vec<usize> = {
            let mut ts = vec![0usize; boundaries.len()];
            let mut count = 0usize;
            let mut bi = 1usize;
            for (i, &b) in shared_mmap[hdr_end..].iter().enumerate() {
                if b == b'#' { count += 1; }
                while bi < boundaries.len() && hdr_end + i + 1 >= boundaries[bi] {
                    ts[bi] = count;
                    bi += 1;
                }
                if bi >= boundaries.len() { break; }
            }
            ts
        };

        // Build chunk descriptors for parallel processing
        let chunks: Vec<(usize, usize, usize)> = (0..boundaries.len() - 1)
            .map(|i| (boundaries[i], boundaries[i+1], boundary_ts[i]))
            .collect();

        // Parallel chunk scan using rayon — Arc<Mmap> is Sync so threads can share
        let results: Vec<Vec<usize>> = chunks.par_iter().map(|&(start, end, start_ts)| {
            let chunk = &shared_mmap[start..end];
            let mut local_indices = Vec::new();
            let mut current_val: Option<VcdValue> = None;
            let mut prev_bit: Option<u8> = None;
            let mut ts_idx = start_ts;
            let mut seen_first_ts = false;

            let mut lp = 0usize;
            while lp < chunk.len() {
                let line_start = lp;
                let line_end = match memchr::memchr(b'\n', &chunk[lp..]) {
                    Some(nl) => { lp += nl; let end = lp; lp += 1; end }
                    None => break,
                };
                let line = &chunk[line_start..line_end];
                if line.is_empty() { continue; }

                let first = line[0];
                if first == b'#' {
                    if seen_first_ts {
                        if let Some(ref val) = current_val {
                            if find_cond_matches(val, prev_bit, &cond) {
                                local_indices.push(ts_idx);
                            }
                            prev_bit = val_to_bit(val);
                        }
                        ts_idx += 1;
                    }
                    seen_first_ts = true;
                } else if first != b'$' && line.len() > id_len {
                    let id_start = line.len() - id_len;
                    if &line[id_start..] == target_id.as_slice() {
                        let val = match first {
                            b'b' => {
                                let ve = id_start.saturating_sub(1);
                                let vs = if ve > 1 && line[ve] == b' ' { &line[1..ve] } else { &line[1..id_start] };
                                VcdValue::Vector(vs.to_vec())
                            }
                            b'r' => {
                                let vs = std::str::from_utf8(&line[1..id_start]).unwrap_or("0");
                                if let Ok(r) = vs.trim().parse::<f64>() { VcdValue::Real(r) } else { continue; }
                            }
                            _ => VcdValue::Bit(first),
                        };
                        current_val = Some(val);
                    }
                }
            }

            if seen_first_ts {
                if let Some(ref val) = current_val {
                    if find_cond_matches(val, prev_bit, &cond) {
                        local_indices.push(ts_idx);
                    }
                }
            }

            local_indices
        }).collect();

        let mut indices: Vec<usize> = results.into_iter().flatten().collect();
        indices.sort();
        indices.dedup();
        Ok(indices)
    }
}

/// Check if current value matches the condition
fn find_cond_matches(val: &VcdValue, prev_bit: Option<u8>, cond: &FindCondition) -> bool {
    match cond {
        FindCondition::Rising => prev_bit == Some(b'0') && val.as_bit() == Some(b'1'),
        FindCondition::Falling => prev_bit == Some(b'1') && val.as_bit() == Some(b'0'),
        FindCondition::High => val.as_bit() == Some(b'1'),
        FindCondition::Low => val.as_bit() == Some(b'0'),
        FindCondition::Value(v) => {
            let bit = val.as_bit();
            bit == Some(*v) || (bit == Some(b'1') && *v == 1) || (bit == Some(b'0') && *v == 0)
        }
        FindCondition::ValueI64(target) => val.to_i64() == Some(*target),
        FindCondition::Neq(v) => {
            let bit = val.as_bit();
            !(bit == Some(*v) || (bit == Some(b'1') && *v == 1) || (bit == Some(b'0') && *v == 0))
        }
        FindCondition::NeqI64(target) => val.to_i64() != Some(*target),
    }
}

fn val_to_bit(val: &VcdValue) -> Option<u8> {
    match val {
        VcdValue::Bit(b) => Some(*b),
        VcdValue::Vector(v) if v.len() == 1 => Some(v[0]),
        _ => None,
    }
}

fn value_to_scalar(val: &VcdValue) -> ScalarValue {
    match val {
        VcdValue::Bit(b) => ScalarValue::Bit(*b),
        VcdValue::Vector(v) => ScalarValue::Vector(v.clone()),
        VcdValue::Real(r) => ScalarValue::Real(*r),
    }
}

