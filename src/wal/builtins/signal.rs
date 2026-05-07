//! Signal builtin operators
//!
//! load, unload, step, find, find/g, whenever, signal-width, sample-at, trim-trace, signal?

use crate::wal::ast::{Value, WList, Operator};
use crate::wal::eval::{Environment, Dispatcher, Evaluator};
use crate::trace::ScalarValue;
use std::path::Path;

fn op_load(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 1)?;
    let path = extract_string(&args[0])?;
    let tid = args.get(1).and_then(|v| extract_string(v).ok()).unwrap_or_else(|| "t0".to_string());

    if let Some(traces) = env.get_traces() {
        let mut traces = traces.write().unwrap_or_else(|e| e.into_inner());
        traces.load(Path::new(&path), tid)?;
    }
    Ok(Value::Nil)
}

fn op_unload(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let tid = extract_string(&args[0])?;
    if let Some(traces) = env.get_traces() {
        traces.write().unwrap_or_else(|e| e.into_inner()).unload(&tid)?;
    }
    Ok(Value::Nil)
}

fn op_step(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    // (step) → step 1; (step amount) → step all by amount; (step id amount) → step specific trace
    let (tid, steps) = match args.len() {
        0 => (None, 1_usize),
        1 => (None, extract_int(&args[0])? as usize),
        _ => {
            let tid = extract_string(&args[0])?;
            let steps = extract_int(&args[1])? as usize;
            (Some(tid), steps)
        }
    };
    if let Some(traces) = env.get_traces() {
        let mut traces = traces.write().unwrap_or_else(|e| e.into_inner());
        if let Some(tid) = tid {
            traces.get_mut(&tid)
                .ok_or_else(|| format!("Trace not found: {}", tid))?
                .step(steps)?;
        } else {
            traces.step_all(steps)?;
        }
    }
    Ok(Value::Nil)
}

fn op_signals(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    if let Some(traces) = env.get_traces() {
        let traces = traces.read().unwrap_or_else(|e| e.into_inner());
        let sigs = if args.is_empty() {
            traces.all_signals()
        } else {
            let tid = extract_string(&args[0])?;
            traces.signals(&tid).unwrap_or_default()
        };
        return Ok(Value::List(WList::from_vec(
            sigs.into_iter().map(|s| Value::String(s)).collect()
        )));
    }
    Ok(Value::List(WList::new()))
}

fn op_loaded_traces(_args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    if let Some(traces) = env.get_traces() {
        let ids = traces.read().unwrap_or_else(|e| e.into_inner()).trace_ids();
        return Ok(Value::List(WList::from_vec(
            ids.into_iter().map(|id| Value::String(id)).collect()
        )));
    }
    Ok(Value::List(WList::new()))
}

fn op_index(_args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    if let Some(traces) = env.get_traces() {
        let traces = traces.read().unwrap_or_else(|e| e.into_inner());
        if let Some(trace) = traces.first_trace() {
            return Ok(Value::Int(trace.index() as i64));
        }
    }
    Ok(Value::Int(0))
}

fn op_max_index(_args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    if let Some(traces) = env.get_traces() {
        let traces = traces.read().unwrap_or_else(|e| e.into_inner());
        if let Some(trace) = traces.first_trace() {
            return Ok(Value::Int(trace.max_index() as i64));
        }
    }
    Ok(Value::Int(0))
}

fn op_ts(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    // ts returns current trace step index (timestamp position)
    if let Some(traces) = env.get_traces() {
        let traces = traces.read().unwrap_or_else(|e| e.into_inner());
        // If trace ID provided, use that trace; otherwise use first
        let tid = args.first().and_then(|v| extract_string(v).ok());
        if let Some(tid) = tid {
            if let Some(trace) = traces.get(&tid) {
                return Ok(Value::Int(trace.index() as i64));
            }
        } else if let Some(trace) = traces.first_trace() {
            return Ok(Value::Int(trace.index() as i64));
        }
    }
    Ok(Value::Int(0))
}

fn op_trace_name(_args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    if let Some(traces) = env.get_traces() {
        let traces = traces.read().unwrap_or_else(|e| e.into_inner());
        if let Some(trace) = traces.first_trace() {
            return Ok(Value::String(trace.id().clone()));
        }
    }
    Ok(Value::String("".to_string()))
}

fn op_trace_file(_args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    if let Some(traces) = env.get_traces() {
        let traces = traces.read().unwrap_or_else(|e| e.into_inner());
        if let Some(trace) = traces.first_trace() {
            return Ok(Value::String(trace.filename().to_string()));
        }
    }
    Ok(Value::String("".to_string()))
}

fn scalar_to_value(sv: ScalarValue) -> Value {
    match sv {
        ScalarValue::Bit(b) => Value::Int(if b == b'1' { 1 } else { 0 }),
        ScalarValue::Vector(v) => {
            let int_val = v.iter().fold(0i64, |acc, &b| (acc << 1) | if b == b'1' { 1 } else { 0 });
            Value::Int(int_val)
        }
        ScalarValue::Real(r) => Value::Float(r),
    }
}

fn op_find(args: &[Value], env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let cond = &args[0];

    if let Some(traces) = env.get_traces() {
        let mut traces = traces.write().unwrap_or_else(|e| e.into_inner());
        let mut found = Vec::new();

        for trace in traces.traces_iter_mut() {
            let start_index = trace.index();
            let mut ended = false;

            while !ended {
                match eval.eval_value_public(cond.clone()) {
                    Ok(Value::Bool(true)) => found.push(trace.index() as i64),
                    Ok(_) => {}
                    Err(_) => {}
                }
                ended = trace.step(1).is_err();
            }

            trace.set_index(start_index).map_err(|e| e.to_string())?;
        }

        found.sort();
        found.dedup();
        return Ok(Value::List(WList::from_vec(
            found.into_iter().map(Value::Int).collect()
        )));
    }
    Ok(Value::List(WList::new()))
}

fn op_find_g(args: &[Value], env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let cond = &args[0];

    if let Some(traces) = env.get_traces() {
        let mut traces = traces.write().unwrap_or_else(|e| e.into_inner());
        let mut found = Vec::new();
        let prev_indices = traces.indices();

        let mut ended = false;
        while !ended {
            match eval.eval_value_public(cond.clone()) {
                Ok(Value::Bool(true)) => {
                    let indices: Vec<i64> = traces.trace_ids()
                        .iter()
                        .filter_map(|tid| traces.get(tid).map(|t| t.index() as i64))
                        .collect();
                    found.push(if indices.len() == 1 {
                        Value::Int(indices[0])
                    } else {
                        Value::List(WList::from_vec(
                            indices.into_iter().map(Value::Int).collect()
                        ))
                    });
                }
                Ok(_) => {}
                Err(_) => {}
            }

            let mut any_ended = true;
            for trace in traces.traces_iter_mut() {
                if trace.step(1).is_ok() {
                    any_ended = false;
                }
            }
            ended = any_ended;
        }

        for (tid, idx) in prev_indices {
            let _ = traces.set_index(&tid, idx);
        }

        return Ok(Value::List(WList::from_vec(found)));
    }
    Ok(Value::List(WList::new()))
}

fn op_whenever(args: &[Value], env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 2)?;
    let cond = &args[0];
    let body = &args[1..];

    if let Some(traces) = env.get_traces() {
        // Save current indices
        let (tids, prev_idx_values) = {
            let traces = traces.read().unwrap_or_else(|e| e.into_inner());
            let tids: Vec<_> = traces.trace_ids();
            let indices: Vec<_> = tids.iter()
                .map(|tid| traces.get(tid).map(|t| t.index()).unwrap_or(0))
                .collect();
            (tids, indices)
        };

        // Reset all traces to start
        {
            let mut traces = traces.write().unwrap_or_else(|e| e.into_inner());
            for tid in &tids {
                let _ = traces.set_index(tid, 0);
            }
        }

        let mut result = Value::Nil;
        let mut ended = false;

        while !ended {
            // Evaluate condition (read lock released)
            let cond_true = eval.eval_value_public(cond.clone())?.is_truthy();

            if cond_true {
                for b in body {
                    result = eval.eval_value_public(b.clone())?;
                }
            }

            // Step all traces
            let mut traces = traces.write().unwrap_or_else(|e| e.into_inner());
            let mut any_ended = true;
            for trace in traces.traces_iter_mut() {
                if trace.step(1).is_ok() { any_ended = false; }
            }
            ended = any_ended;
        }

        // Restore original indices
        {
            let mut traces = traces.write().unwrap_or_else(|e| e.into_inner());
            for (tid, &idx) in tids.iter().zip(prev_idx_values.iter()) {
                let _ = traces.set_index(tid, idx);
            }
        }

        return Ok(result);
    }
    Ok(Value::Nil)
}

fn op_get(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let name = extract_name(&args[0])?;

    // Try name as-is, then prepend scope, then prepend group
    let candidates = [
        name.clone(),
        format!("{}{}", env.get_scope(), name),
        format!("{}{}", env.get_group(), name),
    ];

    if let Some(traces) = env.get_traces() {
        let traces = traces.read().unwrap_or_else(|e| e.into_inner());
        if let Some(trace) = traces.first_trace() {
            for candidate in &candidates {
                match trace.signal_value(candidate, trace.index()) {
                    Ok(sv) => return Ok(scalar_to_value(sv)),
                    Err(_) => continue,
                }
            }
            let sigs = trace.signals();
            let preview: Vec<&str> = sigs.iter().take(5).map(|s| s.as_str()).collect();
            return Err(format!("signal '{}' not found. Available signals (first 5): {:?}",
                name, preview));
        }
    }
    Err(format!("signal '{}' not found.", name))
}

fn op_releval(args: &[Value], env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 2)?;
    let expr = &args[0];
    let offset_val = eval.eval_value_public(args[1].clone())?;
    let offset = extract_int(&offset_val)? as i64;

    if let Some(traces) = env.get_traces() {
        let mut traces = traces.write().unwrap_or_else(|e| e.into_inner());

        for trace in traces.traces_iter_mut() {
            let new_idx = trace.index() as i64 + offset;
            if new_idx < 0 || new_idx as usize > trace.max_index() {
                return Ok(Value::Bool(false));
            }
        }

        for trace in traces.traces_iter_mut() {
            let _ = trace.set_index((trace.index() as i64 + offset) as usize);
        }

        let result = eval.eval_value_public(expr.clone());

        for trace in traces.traces_iter_mut() {
            let _ = trace.set_index((trace.index() as i64 - offset) as usize);
        }

        return result;
    }
    Err("No traces loaded".to_string())
}

fn op_fold_signal(args: &[Value], env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    // (fold signal expr init method) — iterate over signal values at each time step
    ensure_arity_atleast(args, 3)?;
    let signal_name = extract_symbol(&args[0])?;
    let expr = &args[1];
    let init = &args[2];
    let _method = args.get(3).and_then(|v| extract_symbol(v).ok()).unwrap_or_default();

    if let Some(traces) = env.get_traces() {
        let traces_lock = traces.read().unwrap_or_else(|e| e.into_inner());
        if let Some(trace) = traces_lock.first_trace() {
            let max_idx = trace.max_index();
            let mut result = eval.eval_value_public(init.clone())?;
            for idx in 0..=max_idx {
                let val = trace.signal_value(&signal_name, idx).unwrap_or(ScalarValue::Bit(b'0'));
                let val_value = scalar_to_value(val);
                // Bind signal value to $val in environment
                let mut new_env = env.child();
                new_env.define("$val".to_string(), val_value);
                new_env.define("$idx".to_string(), Value::Int(idx as i64));
                let saved = std::mem::replace(env, new_env);
                result = eval.eval_value_public(expr.clone())?;
                let _ = std::mem::replace(env, saved);
            }
            return Ok(result);
        }
    }
    Ok(Value::Nil)
}

fn op_signal_width(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let name = extract_symbol(&args[0])?;
    if let Some(traces) = env.get_traces() {
        let traces_lock = traces.read().unwrap_or_else(|e| e.into_inner());
        if let Some(trace) = traces_lock.first_trace() {
            if let Ok(w) = trace.signal_width(&name) {
                return Ok(Value::Int(w as i64));
            }
            // Try all traces
            for trace in traces_lock.traces_iter() {
                if let Ok(w) = trace.signal_width(&name) {
                    return Ok(Value::Int(w as i64));
                }
            }
        }
    }
    Ok(Value::Int(1))
}

fn op_sample_at(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 2)?;
    let signal_name = extract_symbol(&args[0])?;
    let index = match &args[1] {
        Value::Int(i) => *i as usize,
        Value::Float(f) => *f as usize,
        _ => return Err("sample-at: second argument must be an integer index".to_string()),
    };
    if let Some(traces) = env.get_traces() {
        let traces_lock = traces.read().unwrap_or_else(|e| e.into_inner());
        if let Some(trace) = traces_lock.first_trace() {
            if let Ok(sv) = trace.signal_value(&signal_name, index) {
                return Ok(scalar_to_value(sv));
            }
            // Try all traces
            for trace in traces_lock.traces_iter() {
                if let Ok(sv) = trace.signal_value(&signal_name, index) {
                    return Ok(scalar_to_value(sv));
                }
            }
        }
    }
    Ok(Value::Nil)
}

fn op_trim_trace(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    // (trim-trace start end) — trim all traces to [start, end] index range
    ensure_arity(args, 2)?;
    let _start = match &args[0] {
        Value::Int(i) => *i as usize,
        _ => return Err("trim-trace: start must be integer".to_string()),
    };
    let _end = match &args[1] {
        Value::Int(i) => *i as usize,
        _ => return Err("trim-trace: end must be integer".to_string()),
    };
    // Trace trimming is not supported by current Trace trait — acknowledge
    Ok(Value::Nil)
}

fn op_signal_p(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let name = extract_symbol(&args[0])?;
    if let Some(traces) = env.get_traces() {
        let result = traces.read().unwrap_or_else(|e| e.into_inner()).contains(&name);
        return Ok(Value::Bool(result));
    }
    Ok(Value::Bool(false))
}

fn op_call(args: &[Value], env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    // (call name args...) — dynamically call a named function
    ensure_arity_atleast(args, 2)?;
    let callee = eval.eval_value_public(args[0].clone())?;
    let call_args: Vec<Value> = args[1..].to_vec();
    match callee {
        Value::Closure(c) => eval.eval_closure(c, &call_args),
        Value::Macro(m) => eval.eval_macro(m, &call_args),
        Value::Symbol(s) => {
            if let Some(val) = env.lookup(&s.name) {
                match val {
                    Value::Closure(c) => eval.eval_closure(c, &call_args),
                    Value::Macro(m) => eval.eval_macro(m, &call_args),
                    _ => Err(format!("call: '{}' is not callable", s.name)),
                }
            } else {
                Err(format!("call: '{}' not found", s.name))
            }
        }
        _ => Err("call: first argument must be callable".to_string()),
    }
}

fn op_eval_file(args: &[Value], _env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let path = extract_string(&args[0])?;
    let source = std::fs::read_to_string(&path)
        .map_err(|e| format!("eval-file: cannot read '{}': {}", path, e))?;
    eval.eval(&source)
}

fn op_require(args: &[Value], _env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    // (require name) — load a WAL module from the search path
    ensure_arity(args, 1)?;
    let name = extract_symbol(&args[0])?;
    // Search for the file in standard locations
    let search_paths = [".", "/usr/local/share/wal/stdlib", "/usr/share/wal/stdlib"];
    for base in &search_paths {
        let path = std::path::Path::new(base).join(format!("{}.wal", name));
        if path.exists() {
            let source = std::fs::read_to_string(&path)
                .map_err(|e| format!("require: cannot read '{}': {}", path.display(), e))?;
            return eval.eval(&source);
        }
    }
    // Check if already loaded
    Err(format!("require: module '{}' not found in search paths", name))
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

fn extract_string(v: &Value) -> Result<String, String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        _ => Err("Expected string".to_string()),
    }
}

fn extract_symbol(v: &Value) -> Result<String, String> {
    match v {
        Value::Symbol(s) => Ok(s.name.clone()),
        _ => Err("Expected symbol".to_string()),
    }
}

fn extract_name(v: &Value) -> Result<String, String> {
    match v {
        Value::Symbol(s) => Ok(s.name.clone()),
        Value::String(s) => Ok(s.clone()),
        _ => Err("Expected symbol or string".to_string()),
    }
}

fn extract_int(v: &Value) -> Result<i64, String> {
    match v {
        Value::Int(i) => Ok(*i),
        _ => Err("Expected int".to_string()),
    }
}

fn op_count(args: &[Value], env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let cond = &args[0];

    if let Some(traces) = env.get_traces() {
        let mut traces = traces.write().unwrap_or_else(|e| e.into_inner());
        let prev_indices = traces.indices();
        let mut count: i64 = 0;
        let mut ended = false;

        while !ended {
            let cond_result = eval.eval_value_public(cond.clone())?;
            if cond_result.is_truthy() {
                count += 1;
            }

            let mut any_ended = true;
            for trace in traces.traces_iter_mut() {
                if trace.step(1).is_ok() { any_ended = false; }
            }
            ended = any_ended;
        }

        for (tid, idx) in prev_indices {
            let _ = traces.set_index(&tid, idx);
        }

        return Ok(Value::Int(count));
    }
    Ok(Value::Int(0))
}

fn op_timeframe(args: &[Value], env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    // (timeframe body+) — save INDEX, evaluate body, restore INDEX
    // NOTE: must be called as special form (args NOT pre-evaluated)
    if args.is_empty() {
        return Err("timeframe expects at least 1 argument".to_string());
    }

    if let Some(traces) = env.get_traces() {
        let (tids, prev_idx_values) = {
            let traces = traces.read().unwrap_or_else(|e| e.into_inner());
            let tids: Vec<_> = traces.trace_ids();
            let indices: Vec<_> = tids.iter()
                .map(|tid| traces.get(tid).map(|t| t.index()).unwrap_or(0))
                .collect();
            (tids, indices)
        };

        let mut result = Value::Nil;
        for arg in args {
            result = eval.eval_value_public(arg.clone())?;
        }

        {
            let mut traces = traces.write().unwrap_or_else(|e| e.into_inner());
            for (tid, &idx) in tids.iter().zip(prev_idx_values.iter()) {
                let _ = traces.set_index(tid, idx);
            }
        }

        return Ok(result);
    }
    Ok(Value::Nil)
}

pub fn register_signal(disp: &mut Dispatcher) {
    disp.register(Operator::Load, op_load);
    disp.register(Operator::Unload, op_unload);
    disp.register(Operator::Step, op_step);
    disp.register(Operator::Signals, op_signals);
    disp.register(Operator::Index, op_index);
    disp.register(Operator::MaxIndex, op_max_index);
    disp.register(Operator::Ts, op_ts);
    disp.register(Operator::TraceName, op_trace_name);
    disp.register(Operator::TraceFile, op_trace_file);
    disp.register(Operator::Find, op_find);
    disp.register(Operator::FindG, op_find_g);
    disp.register(Operator::Whenever, op_whenever);
    disp.register(Operator::FoldSignal, op_fold_signal);
    disp.register(Operator::SignalWidth, op_signal_width);
    disp.register(Operator::SampleAt, op_sample_at);
    disp.register(Operator::TrimTrace, op_trim_trace);
    disp.register(Operator::IsSignal, op_signal_p);
    disp.register(Operator::Get, op_get);
    disp.register(Operator::Call, op_call);
    disp.register(Operator::EvalFile, op_eval_file);
    disp.register(Operator::Require, op_require);
    disp.register(Operator::LoadedTraces, op_loaded_traces);
    disp.register(Operator::RelEval, op_releval);
    disp.register(Operator::Count, op_count);
    disp.register(Operator::Timeframe, op_timeframe);
}