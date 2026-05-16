//! Virtual signal builtin operators
//!
//! defsig, new-trace, dump-trace

use crate::wal::ast::{Value, Operator};
use crate::wal::eval::{Environment, Dispatcher, Evaluator};
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

fn value_to_vcd_bit(v: &Value) -> (String, u32) {
    match v {
        Value::Int(i) => {
            let width = 32u32;
            let unsigned = if *i < 0 {
                ((*i as i64).wrapping_abs() as u64) & 0xFFFF_FFFF
            } else {
                *i as u64
            };
            (format!("{:032b}", unsigned), width)
        }
        Value::Bool(b) => (if *b { "1".to_string() } else { "0".to_string() }, 1),
        Value::Float(f) => (if *f == 0.0 { "0".to_string() } else { "1".to_string() }, 1),
        Value::Nil => ("0".to_string(), 1),
        _ => ("0".to_string(), 1),
    }
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

    let mut handles: Vec<(String, u32, String, String)> = Vec::new();
    for (i, (name, expr)) in sigs.iter().enumerate() {
        let id = format!("s{}", i + 1);
        if let Ok(val) = eval.eval_value_public(expr.clone()) {
            let (bits_str, width) = value_to_vcd_bit(&val);
            let vtype = if width > 1 { "reg" } else { "wire" };
            let range = if width > 1 { format!(" [{}:0]", width - 1) } else { String::new() };
            writeln!(file, "$var {} {} {} {}{} $end", vtype, width, id, name, range).ok();
            handles.push((id, width, bits_str, name.clone()));
        }
    }

    writeln!(file, "$upscope $end").ok();
    writeln!(file, "$enddefinitions $end").ok();
    writeln!(file, "$dumpvars").ok();

    for (id, _width, bits_str, _name) in &handles {
        if bits_str.len() == 1 {
            writeln!(file, "{}{}", bits_str, id).ok();
        } else {
            writeln!(file, "b{}{}", bits_str, id).ok();
        }
    }
    writeln!(file, "$end").ok();

    // Phase 3: Batch evaluate and dump
    let mut last_values: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (id, _width, bits_str, _name) in &handles {
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
            writeln!(file, "#{}", idx).ok();

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

            for (i, (_name, expr)) in sigs.iter().enumerate() {
                let id = &handles[i].0;
                let width = handles[i].1;
                if let Ok(val) = eval.eval_value_public(expr.clone()) {
                    let (bits_str, _) = value_to_vcd_bit(&val);
                    let bits = if width > 1 {
                        bits_str.chars().take(width as usize).collect::<String>()
                    } else {
                        bits_str
                    };
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
