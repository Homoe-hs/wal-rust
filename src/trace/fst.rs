//! FST trace implementation using the wellen library backend.
//!
//! Replaces the hand-rolled FST reader with wellen's battle-tested FST parser.

use crate::trace::{Trace, TraceId, ScalarValue, FindCondition, BatchEntry};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use wellen::simple::Waveform;
use wellen::{SignalRef, Signal, SignalValue};

pub struct FstTrace {
    id: TraceId,
    filename: String,
    wf: Waveform,
    timestamps: Vec<u64>,
    name_to_ref: HashMap<String, SignalRef>,
    current_index: usize,
}

fn value_to_scalar(sv: SignalValue) -> ScalarValue {
    match sv {
        SignalValue::Event => ScalarValue::Bit(b'1'),
        SignalValue::Binary(data, _bits) => {
            let bits: Vec<u8> = data.iter()
                .flat_map(|&b| (0..8).rev().map(move |i| (b >> i) & 1))
                .map(|b| if b == 1 { b'1' } else { b'0' })
                .collect();
            if bits.len() == 1 {
                ScalarValue::Bit(bits[0])
            } else {
                ScalarValue::Vector(bits)
            }
        }
        SignalValue::FourValue(data, _bits) | SignalValue::NineValue(data, _bits) => {
            let bits: Vec<u8> = data.to_vec();
            if bits.len() == 1 {
                ScalarValue::Bit(bits[0])
            } else {
                ScalarValue::Vector(bits)
            }
        }
        SignalValue::String(s) => ScalarValue::Vector(s.as_bytes().to_vec()),
        SignalValue::Real(f) => ScalarValue::Real(f),
    }
}

fn sv_as_bit(sv: &SignalValue) -> Option<u8> {
    match sv {
        SignalValue::Binary(data, bits) if *bits == 1 => {
            Some(if data[0] & 0x80 != 0 { b'1' } else { b'0' })
        }
        SignalValue::FourValue(data, bits) | SignalValue::NineValue(data, bits) if *bits == 1 => {
            Some(data[0])
        }
        _ => None,
    }
}

fn sv_to_i64(sv: &SignalValue) -> Option<i64> {
    match sv {
        SignalValue::Binary(data, bits) => {
            if *bits == 0 || *bits > 64 { return None; }
            let mut val: i64 = 0;
            for i in 0..*bits {
                let byte_idx = (i / 8) as usize;
                let bit_idx = 7 - (i % 8);
                if byte_idx < data.len() && (data[byte_idx] >> bit_idx) & 1 != 0 {
                    val |= 1i64 << i;
                }
            }
            Some(val)
        }
        _ => None,
    }
}

fn find_cond_matches(sv: &SignalValue, prev_bit: Option<u8>, cond: &FindCondition) -> bool {
    let curr_bit = sv_as_bit(sv);
    match cond {
        FindCondition::Rising => prev_bit == Some(b'0') && curr_bit == Some(b'1'),
        FindCondition::Falling => prev_bit == Some(b'1') && curr_bit == Some(b'0'),
        FindCondition::High => curr_bit == Some(b'1'),
        FindCondition::Low => curr_bit == Some(b'0'),
        FindCondition::Value(v) => {
            curr_bit == Some(*v) || (curr_bit == Some(b'1') && *v == 1) || (curr_bit == Some(b'0') && *v == 0)
        }
        FindCondition::ValueI64(target) => sv_to_i64(sv) == Some(*target),
        FindCondition::Neq(v) => {
            let bit = sv_as_bit(sv);
            !(bit == Some(*v) || (bit == Some(b'1') && *v == 1) || (bit == Some(b'0') && *v == 0))
        }
        FindCondition::NeqI64(target) => sv_to_i64(sv) != Some(*target),
    }
}

impl FstTrace {
    pub fn load(path: &Path, id: TraceId) -> Result<Self, String> {
        let filename = path.to_string_lossy().to_string();
        let wf = wellen::simple::read(path)
            .map_err(|e| format!("Failed to read FST file {}: {}", filename, e))?;

        let timestamps: Vec<u64> = wf.time_table().to_vec();
        let max_index = if timestamps.is_empty() { 0 } else { timestamps.len() - 1 };

        let mut name_to_ref: HashMap<String, SignalRef> = HashMap::new();
        for var in wf.hierarchy().iter_vars() {
            let full = var.full_name(wf.hierarchy());
            name_to_ref.insert(full, var.signal_ref());
        }

        Ok(FstTrace {
            id,
            filename,
            wf,
            timestamps,
            name_to_ref,
            current_index: 0,
        })
    }
}

impl Trace for FstTrace {
    fn id(&self) -> &TraceId { &self.id }
    fn filename(&self) -> &str { &self.filename }

    fn step(&mut self, steps: usize) -> Result<(), String> {
        let new_idx = self.current_index.saturating_add(steps);
        if new_idx > self.max_index() {
            return Err(format!("step {} exceeds max {}", steps, self.max_index()));
        }
        self.current_index = new_idx;
        Ok(())
    }

    fn signal_value(&self, name: &str, offset: usize) -> Result<ScalarValue, String> {
        if offset >= self.timestamps.len() {
            return Err(format!("offset {} out of range", offset));
        }
        let sig_ref = self.name_to_ref.get(name)
            .ok_or_else(|| format!("Unknown signal: {}", name))?;
        let time_idx = offset as wellen::TimeTableIdx;
        let sig = self.wf.get_signal(*sig_ref)
            .ok_or_else(|| format!("Signal data not loaded: {}", name))?;
        let d_off = sig.get_offset(time_idx)
            .ok_or_else(|| format!("No data at offset {}", offset))?;
        let sv = sig.get_value_at(&d_off, 0);
        Ok(value_to_scalar(sv))
    }

    fn signal_width(&self, name: &str) -> Result<usize, String> {
        for var in self.wf.hierarchy().iter_vars() {
            if var.full_name(self.wf.hierarchy()) == name {
                return Ok(var.length().unwrap_or(1) as usize);
            }
        }
        Err(format!("Unknown signal: {}", name))
    }

    fn signals(&self) -> Vec<String> {
        let mut sigs: Vec<String> = self.wf.hierarchy().iter_vars()
            .map(|v| v.full_name(self.wf.hierarchy()))
            .collect();
        sigs.sort();
        sigs
    }

    fn scopes(&self) -> Vec<String> {
        let mut scopes: Vec<String> = self.wf.hierarchy().iter_scopes()
            .map(|s| s.full_name(self.wf.hierarchy()))
            .collect();
        scopes.sort();
        scopes
    }

    fn max_index(&self) -> usize {
        if self.timestamps.is_empty() { 0 } else { self.timestamps.len() - 1 }
    }

    fn set_index(&mut self, index: usize) -> Result<(), String> {
        if index > self.max_index() {
            return Err(format!("Index {} exceeds max {}", index, self.max_index()));
        }
        self.current_index = index;
        Ok(())
    }

    fn index(&self) -> usize { self.current_index }

    fn find_indices(&self, name: &str, cond: FindCondition) -> Result<Vec<usize>, String> {
        let sig_ref = self.name_to_ref.get(name)
            .ok_or_else(|| format!("Unknown signal: {}", name))?;
        let sig = self.wf.get_signal(*sig_ref)
            .ok_or_else(|| format!("Signal data not loaded: {}", name))?;

        let mut indices = Vec::new();
        let mut prev_bit: Option<u8> = None;

        for (time_idx, sv) in sig.iter_changes() {
            let idx = time_idx as usize;
            if idx > self.max_index() { break; }
            if find_cond_matches(&sv, prev_bit, &cond) {
                indices.push(idx);
            }
            prev_bit = sv_as_bit(&sv);
        }

        Ok(indices)
    }

    fn find_indices_batch(&self, entries: &[BatchEntry]) -> Result<Vec<(String, Vec<usize>)>, String> {
        let mut results = Vec::new();
        for entry in entries {
            match entry {
                BatchEntry::Simple(name, cond) => {
                    let indices = self.find_indices(name, cond.clone()).unwrap_or_default();
                    results.push((name.clone(), indices));
                }
                BatchEntry::And(subs) => {
                    let mut sets: Vec<Vec<usize>> = Vec::new();
                    for (name, cond) in subs {
                        if let Ok(idxs) = self.find_indices(name, cond.clone()) {
                            sets.push(idxs);
                        }
                    }
                    if sets.is_empty() {
                        results.push((format!("__and_{}", results.len()), vec![]));
                    } else {
                        sets.sort_by_key(|s| s.len());
                        let mut base = sets[0].clone();
                        for other in &sets[1..] {
                            let set: HashSet<usize> = other.iter().copied().collect();
                            base.retain(|i| set.contains(i));
                        }
                        results.push((format!("__and_{}", results.len() - 1), base));
                    }
                }
            }
        }
        Ok(results)
    }
}
