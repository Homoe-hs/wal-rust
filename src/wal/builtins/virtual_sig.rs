//! Virtual signal builtin operators
//!
//! defsig, new-trace, dump-trace

use crate::wal::ast::{Value, Operator};
use crate::wal::eval::{Environment, Dispatcher, Evaluator};
use std::fs::File;
use std::io::Write;
use std::path::Path;

fn op_defsig(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 2)?;
    let name = extract_symbol(&args[0])?;
    let expr = &args[1];
    // Store both the expression and register as virtual signal
    env.define(name.clone(), expr.clone());
    env.add_virtual_signal(&name);
    Ok(Value::Nil)
}

fn op_new_trace(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let _name = extract_symbol(&args[0])?;
    Ok(Value::Nil)
}

fn collect_virtual_signals(env: &Environment) -> Vec<(String, Value)> {
    let mut sigs = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for key in env.keys() {
        seen.insert(key.clone());
    }
    // Also collect from global scope (parent chain)
    for key in seen.iter() {
        if crate::wal::ast::Operator::from_str(key).is_some() {
            continue;
        }
        if matches!(key.as_str(), "INDEX" | "MAX-INDEX" | "TS" | "SIGNALS" | "CG" | "CS" | "TRACE-NAME" | "TRACE-FILE") {
            continue;
        }
        if let Some(val) = env.lookup_global(key) {
            sigs.push((key.clone(), val));
        }
    }
    sigs
}

fn op_dump_trace(args: &[Value], env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let path = match &args[0] {
        Value::String(s) => s.clone(),
        Value::Symbol(s) => s.name.clone(),
        _ => return Err("dump-trace: expected path string".to_string()),
    };

    // Collect virtual signal definitions
    let sigs = collect_virtual_signals(env);
    if sigs.is_empty() {
        return Err("dump-trace: no virtual signals defined (use defsig first)".to_string());
    }

    // Determine the timeline from loaded traces
    let max_idx = if let Some(traces) = env.get_traces() {
        let traces = traces.read().unwrap_or_else(|e| e.into_inner());
        traces.trace_ids().iter().filter_map(|tid| traces.get(tid).map(|t| t.max_index())).max().unwrap_or(0)
    } else {
        0
    };

    let mut file = File::create(Path::new(&path))
        .map_err(|e| format!("dump-trace: cannot create '{}': {}", path, e))?;

    writeln!(file, "$version WAL virtual trace $end").ok();
    writeln!(file, "$timescale 1ns $end").ok();
    writeln!(file, "$scope module virtual $end").ok();

    // Write variable declarations and collect value buffers
    let mut handles: Vec<(String, u32)> = Vec::new();
    for (i, (name, _expr)) in sigs.iter().enumerate() {
        let id = format!("s{}", i + 1);
        writeln!(file, "$var wire 1 {} {} $end", id, name).ok();
        handles.push((id, i as u32));
    }

    writeln!(file, "$upscope $end").ok();
    writeln!(file, "$enddefinitions $end").ok();
    writeln!(file, "$dumpvars").ok();

    // Write initial values
    if let Some(traces) = env.get_traces() {
        let mut traces = traces.write().unwrap_or_else(|e| e.into_inner());
        // Save current indices
        let saved = traces.indices();

        // Reset to start
        for tid in traces.trace_ids().clone() {
            let _ = traces.set_index(&tid, 0);
        }

        for (i, (_name, expr)) in sigs.iter().enumerate() {
            let id = &handles[i].0;
            if let Ok(val) = eval.eval_value_public(expr.clone()) {
                let v = value_to_vcd_bit(&val);
                writeln!(file, "b{}{}", v, id).ok();
            }
        }

        // Step through each index and record changes
        for idx in 1..=max_idx {
            writeln!(file, "#{}", idx).ok();

            // Step all traces
            let mut all_ended = true;
            for tid in traces.trace_ids().clone() {
                if let Some(t) = traces.get_mut(&tid) {
                    if t.step(1).is_ok() {
                        all_ended = false;
                    }
                }
            }
            if all_ended { break; }

            for (i, (_name, expr)) in sigs.iter().enumerate() {
                if let Ok(val) = eval.eval_value_public(expr.clone()) {
                    let v = value_to_vcd_bit(&val);
                    let id = &handles[i].0;
                    writeln!(file, "b{}{}", v, id).ok();
                }
            }
        }

        // Restore indices
        for (tid, idx) in saved {
            let _ = traces.set_index(&tid, idx);
        }
    }

    writeln!(file, "$end").ok();
    Ok(Value::String(format!("dumped virtual trace to {}", path)))
}

fn value_to_vcd_bit(v: &Value) -> String {
    match v {
        Value::Int(i) => if *i == 0 { "0".to_string() } else { "1".to_string() },
        Value::Bool(b) => if *b { "1".to_string() } else { "0".to_string() },
        Value::Float(f) => format!("{}", f),
        Value::Nil => "0".to_string(),
        _ => "1".to_string(),
    }
}

fn ensure_arity(args: &[Value], expected: usize) -> Result<(), String> {
    if args.len() != expected {
        return Err(format!("Expected {} arguments, got {}", expected, args.len()));
    }
    Ok(())
}

fn ensure_arity_atleast(args: &[Value], min: usize) -> Result<(), String> {
    if args.len() < min {
        return Err(format!("Expected at least {} arguments, got {}", min, args.len()));
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
    disp.register(Operator::Defsig, op_defsig);
    disp.register(Operator::NewTrace, op_new_trace);
    disp.register(Operator::DumpTrace, op_dump_trace);
}