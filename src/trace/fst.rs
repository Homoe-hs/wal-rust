//! FST trace implementation using the wellen library backend.
//!
//! Replaces the hand-rolled FST reader with wellen's battle-tested FST parser.

use crate::trace::{Trace, TraceId, ScalarValue, FindCondition, BatchEntry};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use wellen::simple::Waveform;
use wellen::{SignalRef, SignalValue};

pub struct FstTrace {
    id: TraceId,
    filename: String,
    wf: RefCell<Waveform>,
    timestamps: Vec<u64>,
    name_to_ref: HashMap<String, SignalRef>,
    current_index: usize,
    /// 首次解码失败的说明(文件损坏/编码不支持);查询路径可能吞掉局部 Err,
    /// 顶层用 fatal_error() 兜底上报,避免"看似正常的错误结果"。
    fatal: RefCell<Option<String>>,
    /// 短名/叶子名/子串解析缓存(与 VcdTrace::resolve_idx 同口径)。
    name_cache: RefCell<HashMap<String, Option<(SignalRef, String)>>>,
}

/// 运行 wellen 的解码调用:捕获 panic 并**抑制其默认打印**
/// (默认 hook 会把 Rust panic 文本直接吐到 stderr,对内网用户像崩溃)。
fn quiet_guard<R>(what: &str, f: impl FnOnce() -> R) -> Result<R, String> {
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    std::panic::set_hook(prev_hook);
    result.map_err(|payload| {
        let msg = if let Some(s) = payload.downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown decoder panic".to_string()
        };
        format!(
            "{}: {}。该 FST 值无法解码(文件可能已损坏:常见于 vcd2fst 处理含\
'向量单字符值'(如 0!/1!)的 VCD 时写出坏文件;请把向量值写成 b<bits> 形式后重新生成)",
            what, msg
        )
    })
}


/// wellen's `SignalValue::to_bit_string()` PANICS for Real/String/Event values
/// (`panic!("Cannot convert ...")`). Guard every call: only the bit-vector
/// variants convert; everything else yields None (handled by the caller).
fn sv_bit_string(sv: &SignalValue) -> Option<String> {
    match sv {
        SignalValue::Binary(..) | SignalValue::FourValue(..) | SignalValue::NineValue(..) => {
            sv.to_bit_string()
        }
        _ => None,
    }
}

fn value_to_scalar(sv: &SignalValue) -> ScalarValue {
    match sv {
        SignalValue::Event => ScalarValue::Bit(b'1'),
        SignalValue::String(s) => ScalarValue::Vector(s.as_bytes().to_vec()),
        SignalValue::Real(f) => ScalarValue::Real(*f),
        _ => {
            // Binary/FourValue/NineValue: wellen packs state codes (0..3 / 0..8),
            // not ASCII chars, and partial bytes are LSB-aligned. Decode through
            // wellen's own to_bit_string() to get canonical VCD chars ('0','1','x','z',...).
            match sv_bit_string(sv) {
                Some(bs) if bs.len() == 1 => ScalarValue::Bit(bs.as_bytes()[0]),
                Some(bs) => ScalarValue::Vector(bs.as_bytes().to_vec()),
                None => ScalarValue::Bit(b'x'),
            }
        }
    }
}

/// 位串是否"确定的全 0/全 1"(含 x/z → None)。与 VCD 侧 `vcd_is_zero` 同口径:
/// 向量 rising = prev 全 0 且 cur 为**确定的非零**(不必全 1)。
fn bytes_is_zero(bs: &[u8]) -> Option<bool> {
    if bs.iter().all(|&b| b == b'0' || b == b'1') {
        Some(bs.iter().all(|&b| b == b'0'))
    } else {
        None
    }
}

fn sv_is_zero(sv: &SignalValue) -> Option<bool> {
    bytes_is_zero(sv_bit_string(sv)?.as_bytes())
}

fn sv_as_bit(sv: &SignalValue) -> Option<u8> {
    let bs = sv_bit_string(sv)?;
    if bs.len() == 1 { Some(bs.as_bytes()[0]) } else { None }
}

fn sv_to_i64(sv: &SignalValue) -> Option<i64> {
    let bs = sv_bit_string(sv)?;
    if bs.is_empty() { return None; }
    let bytes = bs.as_bytes();
    // Match hand-rolled reader semantics: 1-bit x/z treated as 0
    if bytes.len() == 1 {
        return Some(if bytes[0] == b'1' { 1 } else { 0 });
    }
    if !bytes.iter().all(|&b| b == b'0' || b == b'1') { return None; }
    if bytes.len() > 64 {
        // Width beyond 64: value = low 64 bits with the high bits zero
        let hi = &bytes[..bytes.len() - 64];
        if hi.iter().any(|&b| b == b'1') { return None; }
        let lo: u64 = bytes[bytes.len() - 64..].iter().fold(0u64, |acc, &b|
            acc.overflowing_shl(1).0 | if b == b'1' { 1 } else { 0 }
        );
        return Some(lo as i64);
    }
    let mut val: i64 = 0;
    for &b in bytes {
        val = val.overflowing_shl(1).0 | (if b == b'1' { 1 } else { 0 });
    }
    Some(val)
}

fn find_cond_matches(
    sv: &SignalValue,
    prev_bit: Option<u8>,
    prev_val: &mut Option<Vec<u8>>,
    cond: &FindCondition,
) -> bool {
    let curr_bit = sv_as_bit(sv);
    let matched = match cond {
        // 向量语义与 VCD 侧一致: rising = prev 为 0 且 cur 为确定的非零;
        // falling = 反之。只按单个 bit 比较会让**向量信号永远没有边沿**
        // (sv_as_bit 对多位返回 None)——内网 "NE/changes 漂移" 的根因之一。
        FindCondition::Rising => {
            match (prev_val.as_ref().and_then(|p| bytes_is_zero(p)), sv_is_zero(sv)) {
                (Some(true), Some(false)) => true,
                _ => prev_bit == Some(b'0') && curr_bit == Some(b'1'),
            }
        }
        FindCondition::Falling => {
            match (prev_val.as_ref().and_then(|p| bytes_is_zero(p)), sv_is_zero(sv)) {
                (Some(false), Some(true)) => true,
                _ => prev_bit == Some(b'1') && curr_bit == Some(b'0'),
            }
        }
        FindCondition::High => curr_bit == Some(b'1'),
        FindCondition::Low => curr_bit == Some(b'0'),
        FindCondition::Value(v) => {
            if let Some(bit) = curr_bit {
                bit == *v || (bit == b'1' && *v == 1) || (bit == b'0' && *v == 0)
            } else {
                // Vector signal: compare as integer (e.g. 4-bit "0001" == 1)
                sv_to_i64(sv) == Some(*v as i64)
            }
        }
        FindCondition::ValueI64(target) => sv_to_i64(sv) == Some(*target),
        FindCondition::Neq(v) => {
            let bit = sv_as_bit(sv);
            if bit.is_some() {
                !(bit == Some(*v) || (bit == Some(b'1') && *v == 1) || (bit == Some(b'0') && *v == 0))
            } else {
                // Vector signal: compare as integer
                sv_to_i64(sv) != Some(*v as i64)
            }
        }
        FindCondition::NeqI64(target) => sv_to_i64(sv) != Some(*target),
        FindCondition::IsX => match sv_bit_string(sv) {
            Some(bs) => bs.contains('x') || bs.contains('X'),
            None => false,
        },
        FindCondition::IsZ => match sv_bit_string(sv) {
            Some(bs) => bs.contains('z') || bs.contains('Z'),
            None => false,
        },
        FindCondition::Changed => prev_val.as_ref().map(|p| {
            // Initial 'x' may be represented as "x" (bit form) while an
            // explicit x-run is "xxxx…" — same state, not a change.
            fn norm(bs: &[u8]) -> Vec<u8> {
                if bs.iter().all(|&b| b == b'x' || b == b'X') {
                    vec![b'x']
                } else if bs.iter().all(|&b| b == b'z' || b == b'Z') {
                    vec![b'z']
                } else {
                    bs.to_vec()
                }
            }
            norm(p) != norm(&sv_bit_string(sv).unwrap_or_default().into_bytes())
        }).unwrap_or(false),
    };
    if let Some(bs) = sv_bit_string(sv) {
        *prev_val = Some(bs.into_bytes());
    }
    matched
}

impl FstTrace {
    pub fn load(path: &Path, id: TraceId) -> Result<Self, String> {
        let filename = path.to_string_lossy().to_string();
        // wellen panics on some foreign formats (e.g. FSDB with a "similar
        // magic"); turn the abort into a clean error instead of crashing.
        let wf = quiet_guard(&format!("读取 FST 文件 {}", filename), || wellen::simple::read(path))
            .map_err(|e| format!("Failed to read FST file {}: {}", filename, e))?
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
            wf: RefCell::new(wf),
            timestamps,
            name_to_ref,
            current_index: 0,
            fatal: RefCell::new(None),
            name_cache: RefCell::new(HashMap::new()),
        })
    }

    /// 解析信号名: 精确 → 叶子名(短名/无点) → 子串;结果缓存(ref + 全名)。
    fn resolve_cached(&self, name: &str) -> Option<(SignalRef, String)> {
        if let Some(r) = self.name_to_ref.get(name) {
            return Some((*r, name.to_string()));
        }
        if let Some(c) = self.name_cache.borrow().get(name) {
            return c.clone();
        }
        fn leaf(s: &str) -> &str { s.rsplitn(2, '.').next().unwrap_or("") }
        let mut hit: Option<(SignalRef, String)> = None;
        let mut sub: Option<(SignalRef, String)> = None;
        {
            let wf = self.wf.borrow();
            let h = wf.hierarchy();
            for var in h.iter_vars() {
                let full = var.full_name(h);
                if (name.len() <= 8 || !name.contains('.')) && leaf(&full) == name {
                    hit = Some((var.signal_ref(), full));
                    break;
                }
                if sub.is_none() && full.contains(name) {
                    sub = Some((var.signal_ref(), full));
                }
            }
        }
        let found = hit.or(sub);
        self.name_cache.borrow_mut().insert(name.to_string(), found.clone());
        found
    }

    /// Resolve a signal ref and load its data on demand (wellen loads lazily)
    fn resolve_ref(&self, name: &str) -> Result<SignalRef, String> {
        self.resolve_cached(name).map(|(r, _)| r)
            .ok_or_else(|| format!("Unknown signal: {}", name))
    }

    fn ensure_loaded(&self, name: &str) -> Result<SignalRef, String> {
        if self.timestamps.is_empty() {
            // Empty FST (e.g. whole simulation dumped off): wellen's FST reader has no
            // time table and panics inside load_signals — refuse up front instead.
            return Err(format!("No waveform data in FST file: {}", name));
        }
        let sig_ref = self.resolve_ref(name)?;
        let mut wf = self.wf.borrow_mut();
        if wf.get_signal(sig_ref).is_none() {
            // wellen may panic on foreign/odd value encodings (FST write
            // variants); turn that into a clean query-time error and remember it
            // (查询路径可能吞掉局部 Err,顶层用 fatal_error() 兜底)。
            let r = quiet_guard(
                &format!("FST 信号 {} 的数据", name),
                || wf.load_signals(&[sig_ref]),
            );
            if let Err(e) = r {
                let mut slot = self.fatal.borrow_mut();
                if slot.is_none() {
                    *slot = Some(e.clone());
                }
                return Err(e);
            }
        }
        Ok(sig_ref)
    }
}

impl Trace for FstTrace {
    fn id(&self) -> &TraceId { &self.id }

    fn fatal_error(&self) -> Option<String> {
        self.fatal.borrow().clone()
    }
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
        let sig_ref = self.ensure_loaded(name)?;
        let wf = self.wf.borrow();
        let time_idx = offset as wellen::TimeTableIdx;
        let sig = wf.get_signal(sig_ref)
            .ok_or_else(|| format!("Signal data not loaded: {}", name))?;
        let d_off = match sig.get_offset(time_idx) {
            Some(d) => d,
            // No change at or before this index: VCD semantics = initial value 'x'
            None => {
                let width = self.signal_width(name).unwrap_or(1);
                return Ok(if width == 1 {
                    ScalarValue::Bit(b'x')
                } else {
                    ScalarValue::Vector(vec![b'x'; width])
                });
            }
        };
        // Several changes can share the same time-table index (delta cycles).
        // The value AT the index is the LAST change, same as the VCD backend.
        let elem = d_off.elements.saturating_sub(1);
        let sv = sig.get_value_at(&d_off, elem);
        Ok(value_to_scalar(&sv))
    }

    fn signal_width(&self, name: &str) -> Result<usize, String> {
        let want = self.resolve_ref(name)?;
        let wf = self.wf.borrow();
        for var in wf.hierarchy().iter_vars() {
            if var.signal_ref() == want {
                return Ok(var.length().unwrap_or(1) as usize);
            }
        }
        Err(format!("Unknown signal: {}", name))
    }

    fn resolve_name(&self, name: &str) -> Option<String> {
        self.resolve_cached(name).map(|(_, n)| n)
    }

    fn signals(&self) -> Vec<String> {
        let wf = self.wf.borrow();
        let mut sigs: Vec<String> = wf.hierarchy().iter_vars()
            .map(|v| v.full_name(wf.hierarchy()))
            .collect();
        sigs.sort();
        sigs
    }

    fn scopes(&self) -> Vec<String> {
        let wf = self.wf.borrow();
        let mut scopes: Vec<String> = wf.hierarchy().iter_scopes()
            .map(|s| s.full_name(wf.hierarchy()))
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
        let sig_ref = self.ensure_loaded(name)?;
        let wf = self.wf.borrow();
        let sig = wf.get_signal(sig_ref)
            .ok_or_else(|| format!("Signal data not loaded: {}", name))?;

        // Edge conditions (Rising/Falling/Changed) only hold at change points.
        // Level conditions (High/Low/Value/Neq/IsX/IsZ) hold for the whole interval
        // between changes — expand to every timestamp, matching the VCD backend's
        // semantics (a signal held high for N timestamps counts N times).
        let is_edge = matches!(
            &cond,
            FindCondition::Rising | FindCondition::Falling | FindCondition::Changed
        );

        // Collapse multiple changes at the same time-table index (delta cycles)
        // to their LAST value, so conditions use per-index semantics identical to
        // the VCD backend (value at index = last change at that index).
        let mut collapsed: Vec<(usize, SignalValue)> = Vec::new();
        for (time_idx, sv) in sig.iter_changes() {
            let idx = time_idx as usize;
            if idx > self.max_index() { break; }
            match collapsed.last_mut() {
                Some(last) if last.0 == idx => last.1 = sv,
                _ => collapsed.push((idx, sv)),
            }
        }

        let mut changes: Vec<(usize, bool)> = Vec::new();
        let mut prev_bit: Option<u8> = None;
        let mut prev_val: Option<Vec<u8>> = None;
        // Initial value before the first change: 'x' (same as get_offset()→None).
        // Level conditions (is-x/Neq/…) hold during the x-run from index 0 and
        // edge conditions see the x as their previous value (x→1 IS a change;
        // x is neither 0 nor 1 so x→1 is never a rising/falling edge), exactly
        // like the VCD backend.
        // 索引 0 没有前驱: 首个变更点就在 0 时不把初值当 prev
        // (与 VCD 的 eval_change_list / 逐拍 op_changes 一致)
        let first_at_zero = collapsed.first().map(|(i, _)| *i) == Some(0);
        if !first_at_zero {
            let xv = SignalValue::FourValue(&[2u8], 1);
            let init_matched = find_cond_matches(&xv, prev_bit, &mut prev_val, &cond);
            prev_bit = sv_as_bit(&xv);
            if !is_edge && init_matched {
                changes.push((0, true));
            }
        }
        for (idx, sv) in &collapsed {
            let matched = find_cond_matches(sv, prev_bit, &mut prev_val, &cond);
            prev_bit = sv_as_bit(sv);
            changes.push((*idx, matched));
        }

        let mut indices = Vec::new();
        for (i, &(idx, matched)) in changes.iter().enumerate() {
            if !matched { continue; }
            if is_edge {
                indices.push(idx);
            } else {
                let end = changes.get(i + 1).map(|&(n, _)| n).unwrap_or(self.max_index() + 1);
                indices.extend(idx..end.min(self.max_index() + 1));
            }
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

    fn timestamp_at(&self, index: usize) -> Option<u64> {
        self.timestamps.get(index).copied()
    }

    fn timescale_exp(&self) -> Option<i8> {
        let wf = self.wf.borrow();
        let ts = wf.hierarchy().timescale()?;
        let exp = match ts.unit {
            wellen::TimescaleUnit::Seconds => 0,
            wellen::TimescaleUnit::MilliSeconds => -3,
            wellen::TimescaleUnit::MicroSeconds => -6,
            wellen::TimescaleUnit::NanoSeconds => -9,
            wellen::TimescaleUnit::PicoSeconds => -12,
            wellen::TimescaleUnit::FemtoSeconds => -15,
            wellen::TimescaleUnit::AttoSeconds => -18,
            wellen::TimescaleUnit::ZeptoSeconds => -21,
            wellen::TimescaleUnit::Unknown => return None,
        };
        Some(exp)
    }

    fn change_points(&self, name: &str) -> Result<Vec<(usize, ScalarValue)>, String> {
        let sig_ref = self.ensure_loaded(name)?;
        let wf = self.wf.borrow();
        let sig = wf.get_signal(sig_ref)
            .ok_or_else(|| format!("Signal data not loaded: {}", name))?;
        // Per-index change points, like the VCD backend: collapse multiple
        // changes at the same time-table index (delta cycles) to their LAST
        // value, then drop entries whose value did not change vs the previous
        // index (glitches that return to the prior value are not change points).
        let mut collapsed: Vec<(usize, ScalarValue)> = Vec::new();
        for (time_idx, sv) in sig.iter_changes() {
            let idx = time_idx as usize;
            if idx > self.max_index() { break; }
            let sv = value_to_scalar(&sv);
            match collapsed.last_mut() {
                Some((last_idx, last_sv)) if *last_idx == idx => *last_sv = sv,
                _ => collapsed.push((idx, sv)),
            }
        }
        let mut out: Vec<(usize, ScalarValue)> = Vec::new();
        for (idx, sv) in collapsed {
            if out.last().map(|(_, prev)| prev != &sv).unwrap_or(true) {
                out.push((idx, sv));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// wellen 的 to_bit_string 对 Real/String/Event 会 panic;sv_bit_string 必须
    /// 返回 None 而不是崩溃(内网 #1 FST 向量 panic 的兜底)。
    #[test]
    fn test_sv_bit_string_no_panic_on_non_bitvector() {
        let real = SignalValue::Real(1.5);
        assert!(sv_bit_string(&real).is_none());
        let s = SignalValue::String("abc");
        assert!(sv_bit_string(&s).is_none());
        assert!(sv_bit_string(&SignalValue::Event).is_none());
        // 位向量正常转换(LSB 对齐: 0b0101 的低 4 位 = "0101")
        let bin = SignalValue::Binary(&[0b0000_0101u8], 4);
        assert_eq!(sv_bit_string(&bin).as_deref(), Some("0101"));
        // 标量解码路径也不 panic
        assert!(matches!(value_to_scalar(&real), ScalarValue::Real(_)));
    }
}
