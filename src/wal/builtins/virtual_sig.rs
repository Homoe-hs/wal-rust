//! Virtual signal builtin operators
//!
//! defsig, new-trace, dump-trace

use crate::wal::ast::{Value, Operator};
use crate::wal::eval::{Environment, Dispatcher, Evaluator};
use crate::trace::Trace;
use std::fs::File;
use std::io::Write;
use std::path::Path;

fn op_new_trace(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let _name = extract_symbol(&args[0])?;
    Ok(Value::Nil)
}

fn collect_virtual_signal_args(env: &Environment) -> Vec<(String, Value)> {
    let mut sigs = Vec::new();
    for name in env.virtual_signal_names() {
        if let Some(val) = env.lookup(&name) {
            sigs.push((name, val));
        }
    }
    sigs
}

fn int_bit_width(i: i64) -> u32 {
    if i < 0 { 64 } else {
        (64 - (i as u64).leading_zeros()).max(1)
    }
}

/// Fit a bit-string to `width`: left-pad with '0' / keep the low `width` bits.
fn fit_bits(bits: &str, width: u32) -> String {
    let w = width as usize;
    let chars: Vec<char> = bits.chars().collect();
    if chars.len() < w {
        let mut out = "0".repeat(w - chars.len());
        out.extend(chars);
        out
    } else if chars.len() > w {
        chars[chars.len() - w..].iter().collect()
    } else {
        bits.to_string()
    }
}

fn value_to_vcd_bit(v: &Value) -> (String, u32) {
    match v {
        Value::Int(i) => {
            // Dynamic width: the value's bit length (45-bit vectors must not be
            // truncated to a hard-coded 32 bits).
            let width = int_bit_width(*i);
            (format!("{:0width$b}", *i as u64, width = width as usize), width)
        }
        Value::Bool(b) => (if *b { "1".to_string() } else { "0".to_string() }, 1),
        Value::Float(f) => (if *f == 0.0 { "0".to_string() } else { "1".to_string() }, 1),
        // x/z-aware get returns bit-strings for vectors with unknown bits;
        // 其它字符串(如信号名)不是波形值 → 置 x 串(调用方会跳过并告警)。
        Value::String(s) => {
            if s.chars().all(|c| matches!(c, '0' | '1' | 'x' | 'z' | 'X' | 'Z')) {
                (s.to_ascii_lowercase(), s.len() as u32)
            } else {
                ("x".repeat(s.len().max(1)), s.len().max(1) as u32)
            }
        }
        Value::Nil => ("0".to_string(), 1),
        _ => ("0".to_string(), 1),
    }
}

/// Best-effort width of a referenced signal inside the loaded traces
/// (exact name, then scoped candidates, then fuzzy fallback — like `get`).
fn sig_width_in_traces(name: &str, env: &Environment) -> Option<u32> {
    let traces = env.get_traces()?;
    let traces = traces.read().ok()?;
    for tr in traces.traces_iter() {
        let sigs = tr.signals();
        let candidates = [
            name.to_string(),
            format!("{}{}", env.get_scope(), name),
            format!("{}{}", env.get_group(), name),
        ];
        let resolved = candidates.iter()
            .find(|c| sigs.iter().any(|s| s == *c))
            .cloned()
            .or_else(|| {
                crate::wal::builtins::signal::fuzzy_match_signal(name, &sigs).0.cloned()
            });
        if let Some(r) = resolved {
            if let Ok(w) = tr.signal_width(&r) {
                return Some(w as u32);
            }
        }
    }
    None
}

/// Extract all (get "signal_name") references from an expression tree.
fn extract_get_signals(expr: &Value) -> Vec<String> {
    let mut sigs = Vec::new();
    extract_get_signals_inner(expr, &mut sigs);
    sigs
}
fn extract_get_signals_inner(expr: &Value, out: &mut Vec<String>) {
    match expr {
        Value::List(lst) => {
            if lst.len() == 2 {
                if let Value::Symbol(s) = &lst[0] {
                    if s.name == "get" {
                        if let Value::String(name) = &lst[1] {
                            out.push(name.clone());
                            return;
                        }
                    }
                }
            }
            for item in lst.iter() {
                extract_get_signals_inner(item, out);
            }
        }
        _ => {}
    }
}

/// Pre-load signal change data for all referenced signals.
/// This populates signal_cache with full_scan=true in VcdTrace,
/// so subsequent signal_value() calls use O(log C) binary search.
fn preload_signal_changes(eval: &mut Evaluator, sigs: &[(String, Value)]) {
    let mut all_signals: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (_, expr) in sigs {
        for sig in extract_get_signals(expr) {
            all_signals.insert(sig);
        }
    }
    // Trigger find_indices for each signal — populates signal_cache
    for sig in &all_signals {
        // find all non-zero occurrences (this triggers a full parallel scan)
        let _ = eval.eval(&format!("(find (!= (get {:?}) 0))", sig));
    }
}

fn op_dump_trace(args: &[Value], env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let path = match &args[0] {
        Value::String(s) => s.clone(),
        Value::Symbol(s) => s.name.clone(),
        _ => return Err("dump-trace: expected path string".to_string()),
    };

    let sigs = collect_virtual_signal_args(env);
    if sigs.is_empty() {
        return Err("dump-trace: no virtual signals defined (use defsig first)".to_string());
    }
    if path.to_ascii_lowercase().ends_with(".fst") {
        return Err(format!(
            "dump-trace: '{}': 只支持 VCD 输出(.fst 暂不支持; 请写 .vcd, 或先用外部工具转换)",
            path
        ));
    }

    let max_idx = if let Some(traces) = env.get_traces() {
        let traces = traces.read().unwrap_or_else(|e| e.into_inner());
        traces.trace_ids().iter().filter_map(|tid| traces.get(tid).map(|t| t.max_index())).max().unwrap_or(0)
    } else {
        0
    };

    // Phase 1: Pre-load signal changes into signal_cache (full_scan)
    // This makes subsequent signal_value() calls O(log C) instead of O(N).
    eprintln!("Pre-loading signal changes...");
    preload_signal_changes(eval, &sigs);

    // Phase 2: Determine widths and write VCD header
    let mut file = File::create(Path::new(&path))
        .map_err(|e| format!("dump-trace: cannot create '{}': {}", path, e))?;

    writeln!(file, "$version WAL virtual trace $end").ok();
    writeln!(file, "$timescale 1ns $end").ok();
    writeln!(file, "$scope module virtual $end").ok();

    // (id, width, 初值位串, 名字, 表达式)
    let mut handles: Vec<(String, u32, String, String, Value)> = Vec::new();
    for (i, (name, expr)) in sigs.iter().enumerate() {
        let id = format!("s{}", i + 1);
        if let Ok(val) = eval.eval_value_public(expr.clone()) {
            // 非波形值(信号名等字符串 / 闭包)没有 VCD 表示 → 跳过并告警
            let representable = match &val {
                Value::String(s) => s.chars().all(|c| matches!(c, '0' | '1' | 'x' | 'z' | 'X' | 'Z')),
                Value::Closure(_) | Value::Macro(_) => false,
                _ => true,
            };
            if !representable {
                eprintln!(
                    "warning: dump-trace: 跳过虚拟信号 '{}' — 其值 {:?} 不是波形值(位串/整数)",
                    name, val
                );
                continue;
            }
            let (mut bits_str, mut width) = value_to_vcd_bit(&val);
            // Widen by the underlying signal's declared width (a 45-bit value
            // read as int at the first sample must not be declared as 32 bits).
            for sig in extract_get_signals(expr) {
                if let Some(w) = sig_width_in_traces(&sig, env) {
                    width = width.max(w);
                }
            }
            bits_str = fit_bits(&bits_str, width);
            let vtype = if width > 1 { "reg" } else { "wire" };
            let range = if width > 1 { format!(" [{}:0]", width - 1) } else { String::new() };
            writeln!(file, "$var {} {} {} {}{} $end", vtype, width, id, name, range).ok();
            handles.push((id, width, bits_str, name.clone(), expr.clone()));
        }
    }

    writeln!(file, "$upscope $end").ok();
    writeln!(file, "$enddefinitions $end").ok();
    writeln!(file, "$dumpvars").ok();

    for (id, _width, bits_str, _name, _expr) in &handles {
        if bits_str.len() == 1 {
            writeln!(file, "{}{}", bits_str, id).ok();
        } else {
            writeln!(file, "b{}{}", bits_str, id).ok();
        }
    }
    writeln!(file, "$end").ok();

    // Phase 3: Batch evaluate and dump
    let mut last_values: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (id, _width, bits_str, _name, _expr) in &handles {
        last_values.insert(id.clone(), bits_str.clone());
    }

    if let Some(traces_rc) = env.get_traces() {
        let saved = {
            let mut traces = traces_rc.write().unwrap_or_else(|e| e.into_inner());
            let saved = traces.indices();
            for tid in traces.trace_ids().clone() {
                let _ = traces.set_index(&tid, 0);
            }
            saved
        };

        eprintln!("Dumping {} indices, {} virtual signals...", max_idx, sigs.len());
        for idx in 1..=max_idx {
            // 用源波形的时间戳(而不是索引号), 否则导出的时标与原始波形对不上
            let ts = {
                let traces = traces_rc.read().unwrap_or_else(|e| e.into_inner());
                traces.first_trace().and_then(|t| t.timestamp_at(idx)).unwrap_or(idx as u64)
            };
            writeln!(file, "#{}", ts).ok();

            let mut all_ended = true;
            {
                let mut traces = traces_rc.write().unwrap_or_else(|e| e.into_inner());
                for tid in traces.trace_ids().clone() {
                    if let Some(t) = traces.get_mut(&tid) {
                        if t.step(1).is_ok() { all_ended = false; }
                    }
                }
            }
            if all_ended { break; }

            for (id, width, _init, _name, expr) in handles.iter() {
                let id = id;
                let width = *width;
                if let Ok(val) = eval.eval_value_public(expr.clone()) {
                    let (bits_str, _) = value_to_vcd_bit(&val);
                    // Fit to the declared width (left-pad / keep low bits), so
                    // multi-bit virtual signals keep every bit and change
                    // detection is not defeated by truncation.
                    let bits = fit_bits(&bits_str, width);
                    if last_values.get(id).map_or(true, |last| *last != bits) {
                        if width > 1 {
                            writeln!(file, "b{}{}", bits, id).ok();
                        } else {
                            writeln!(file, "{}{}", bits, id).ok();
                        }
                        last_values.insert(id.clone(), bits);
                    }
                }
            }
        }

        let mut traces = traces_rc.write().unwrap_or_else(|e| e.into_inner());
        for (tid, idx) in saved {
            let _ = traces.set_index(&tid, idx);
        }
    }

    Ok(Value::String(format!("dumped virtual trace to {}", path)))
}

fn ensure_arity(args: &[Value], expected: usize) -> Result<(), String> {
    if args.len() != expected {
        return Err(format!("Expected {} arguments, got {}", expected, args.len()));
    }
    Ok(())
}

fn extract_symbol(v: &Value) -> Result<String, String> {
    match v {
        Value::Symbol(s) => Ok(s.name.clone()),
        _ => Err("Expected symbol".to_string()),
    }
}

pub fn register_virtual(disp: &mut Dispatcher) {
    disp.register(Operator::NewTrace, op_new_trace);
    disp.register(Operator::DumpTrace, op_dump_trace);
}
