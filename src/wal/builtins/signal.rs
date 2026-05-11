//! Signal builtin operators
//!
//! load, unload, step, find, find/g, whenever, signal-width, sample-at, trim-trace, signal?

use crate::wal::ast::{Value, WList, Operator, Symbol};
use crate::wal::eval::{Environment, Dispatcher, Evaluator, resolve_signal_name};
use crate::trace::{FindCondition, ScalarValue, Trace, TraceContainer};
use std::path::Path;

fn op_load(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity_atleast(args, 1)?;
    let path = extract_string(&args[0])?;
    let tid = if let Some(v) = args.get(1) {
        extract_string(v)?
    } else {
        // Auto-generate ID using scheme t0, t1, t2...
        let count = env.get_traces()
            .map(|t| t.read().unwrap_or_else(|e| e.into_inner()).trace_ids().len())
            .unwrap_or(0);
        format!("t{}", count)
    };

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
    // Returns #f if the end of any loaded trace is reached (per WAL spec)
    // Negative steps go backward (via set_index)
    let parse_steps = |v: &Value| -> Result<i64, String> {
        let n = extract_int(v)?;
        Ok(n)
    };

    let (tid, steps) = match args.len() {
        0 => (None, 1i64),
        1 => (None, parse_steps(&args[0])?),
        _ => {
            let tid = extract_string(&args[0])?;
            let steps = parse_steps(&args[1])?;
            (Some(tid), steps)
        }
    };

    let do_step = |trace: &mut dyn Trace, steps: i64| -> bool {
        if steps >= 0 {
            trace.step(steps as usize).is_ok()
        } else {
            let new_idx = (trace.index() as i64 + steps).max(0) as usize;
            trace.set_index(new_idx).is_ok()
        }
    };

    let do_step_all = |traces: &mut TraceContainer, steps: i64| -> bool {
        let mut all_ok = true;
        for trace in traces.traces_iter_mut() {
            if !do_step(&mut **trace, steps) {
                all_ok = false;
            }
        }
        all_ok
    };

    if let Some(traces) = env.get_traces() {
        let mut traces = traces.write().unwrap_or_else(|e| e.into_inner());
        let ok = if let Some(tid) = tid {
            match traces.get_mut(&tid) {
                Some(trace) => do_step(&mut **trace, steps),
                None => return Err(format!("Trace not found: {}", tid)),
            }
        } else {
            do_step_all(&mut traces, steps)
        };
        return Ok(Value::Bool(ok));
    }
    Ok(Value::Bool(true))
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
    ensure_arity_atleast(args, 1)?;
    let cond = &args[0];
    let max_results = args.get(1).and_then(|v| match v { Value::Int(n) => Some(*n as usize), _ => None }).unwrap_or(usize::MAX);

    // Fast path: simple condition → find_indices
    if let Some(result) = try_find_indices_simple(cond, max_results, env) {
        return result;
    }

    // Fallback: step-by-step scan
    if let Some(traces) = env.get_traces() {
        let mut traces = traces.write().unwrap_or_else(|e| e.into_inner());
        let mut found = Vec::new();

        for trace in traces.traces_iter_mut() {
            let start_index = trace.index();
            let mut ended = false;

            while !ended && found.len() < max_results {
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
        if found.len() > max_results {
            found.truncate(max_results);
        }
        return Ok(Value::List(WList::from_vec(
            found.into_iter().map(Value::Int).collect()
        )));
    }
    Ok(Value::List(WList::new()))
}

/// Try fast path for simple (= (get "sig") val) condition using find_indices
fn try_find_indices_simple(cond: &Value, max_results: usize, env: &mut Environment) -> Option<Result<Value, String>> {
    let (sig_name, target) = parse_simple_condition(cond)?;
    let cond_enum = if target <= 1 && target >= 0 {
        FindCondition::Value(target as u8)
    } else {
        FindCondition::ValueI64(target)
    };

    let traces = env.get_traces()?;
    let first_trace_info = {
        let t = traces.read().ok()?;
        let tr = t.first_trace()?;
        let sigs = tr.signals();
        let resolved = resolve_signal_name(&sig_name, &sigs)
            .unwrap_or_else(|| sig_name.clone());
        (tr.id().clone(), resolved)
    };
    let (tid, resolved) = first_trace_info;

    let indices = {
        let t = traces.read().ok()?;
        t.find_indices(&resolved, cond_enum).ok()?
    };

    let limited: Vec<Value> = indices.into_iter()
        .take(max_results)
        .map(|i| Value::Int(i as i64))
        .collect();

    Some(Ok(Value::List(WList::from_vec(limited))))
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

        let mut result = Value::Nil;

        // Fast path: try simple condition (= (get "sig") val) → use find_indices
        if let Some((sig_name, target)) = parse_simple_condition(cond) {
            let cond_enum = if target <= 1 && target >= 0 {
                FindCondition::Value(target as u8)
            } else {
                FindCondition::ValueI64(target)
            };
            if let Some(trace) = {
                let t = traces.read().unwrap_or_else(|e| e.into_inner());
                t.first_trace().map(|tr| (tr.id().clone(), tr.signals()))
            } {
                let (tid, sigs) = trace;
                let (resolved, _candidates) = fuzzy_match_signal(&sig_name, &sigs);
                if let Some(resolved) = resolved {
                    if let Ok(indices) = {
                        let t = traces.read().unwrap_or_else(|e| e.into_inner());
                        t.find_indices(resolved, cond_enum)
                    } {
                        for &idx in &indices {
                            {
                                let mut t = traces.write().unwrap_or_else(|e| e.into_inner());
                                let _ = t.set_index(&tid, idx);
                            }
                            for b in body {
                                result = eval.eval_value_public(b.clone())?;
                            }
                        }
                        // Restore original indices
                        {
                            let mut t = traces.write().unwrap_or_else(|e| e.into_inner());
                            for (tid, &idx) in tids.iter().zip(prev_idx_values.iter()) {
                                let _ = t.set_index(tid, idx);
                            }
                        }
                        return Ok(result);
                    }
                }
            }
        }

        // Fallback: step-by-step iteration
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

/// Signal name resolution: returns (selected_signal, all_substring_matches for warning)
/// Returns empty candidates vec for exact/suffix matches (no ambiguity).
fn fuzzy_match_signal<'a>(name: &str, signals: &'a [String]) -> (Option<&'a String>, Vec<&'a String>) {
    // 1. Exact match
    if let Some(s) = signals.iter().find(|s| s.as_str() == name) {
        return (Some(s), vec![]);
    }
    // 2. Suffix match: signal ends with .name
    let dot_name = format!(".{}", name);
    let suffix: Vec<&'a String> = signals.iter().filter(|s| s.ends_with(&dot_name)).collect();
    if suffix.len() == 1 { return (Some(suffix[0]), vec![]); }
    if suffix.len() > 1 { return (Some(suffix[0]), suffix); }

    // 3. Last component match for short names
    if name.len() <= 8 || !name.contains('.') {
        let last_comp: Vec<&'a String> = signals.iter()
            .filter(|s| s.rsplitn(2, '.').next().unwrap_or("") == name)
            .collect();
        if last_comp.len() == 1 { return (Some(last_comp[0]), vec![]); }
        if last_comp.len() > 1 { return (Some(last_comp[0]), last_comp); }
    }

    // 4. Substring match
    let sub: Vec<&'a String> = signals.iter().filter(|s| s.contains(name)).collect();
    if sub.len() == 1 { return (Some(sub[0]), vec![]); }
    if sub.len() > 1 { return (Some(sub[0]), sub); }

    (None, vec![])
}

/// Parse a simple condition expression like (= (get "signal") N)
fn parse_simple_condition(expr: &Value) -> Option<(String, i64)> {
    let lst = match expr {
        Value::List(lst) if lst.len() == 3 => lst,
        _ => return None,
    };
    let op = match &lst.0[0] {
        Value::Symbol(s) => s.name.as_str(),
        _ => return None,
    };
    if op != "=" { return None; }
    for (a, b) in &[(0, 1), (1, 0), (1, 2)] {
        if let Value::List(inner) = &lst.0[*a] {
            if inner.len() == 2 {
                if let Value::Symbol(fn_sym) = &inner.0[0] {
                    if fn_sym.name == "get" {
                        let sig = match &inner.0[1] {
                            Value::String(s) => s.clone(),
                            Value::Symbol(s) => s.name.clone(),
                            _ => continue,
                        };
                        let val = match &lst.0[*b] {
                            Value::Int(i) => *i,
                            _ => continue,
                        };
                        return Some((sig, val));
                    }
                }
            }
        }
    }
    None
}

fn op_get(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let name = extract_name(&args[0])?;

    // Try exact candidates with scope/group prepended
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
            // Fuzzy fallback: try suffix / substring matching
            let sigs = trace.signals();
            let (matched, candidates) = fuzzy_match_signal(&name, &sigs);
            if candidates.len() > 1 {
                log::warn!("signal '{}' is ambiguous: matches {:?}, using '{}'",
                    name, &candidates[..candidates.len().min(5)], matched.as_ref().map(|s| s.as_str()).unwrap_or("?"));
            }
            if let Some(matched) = matched {
                if let Ok(sv) = trace.signal_value(matched, trace.index()) {
                    return Ok(scalar_to_value(sv));
                }
            }
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
    // golden-compatible: (fold/signal f acc stop signal)
    //   f     = function (fn [acc val] ...) applied at each step
    //   acc   = initial accumulator value
    //   stop  = condition expression; fold stops when truthy
    //   signal = signal name
    ensure_arity(args, 4)?;
    let f = &args[0];
    let mut acc = eval.eval_value_public(args[1].clone())?;
    let stop = &args[2];
    let signal_name = extract_symbol(&args[3])?;

    if let Some(traces) = env.get_traces() {
        // Save current trace positions
        let saved_positions: Vec<(String, usize)> = {
            let t = traces.read().unwrap_or_else(|e| e.into_inner());
            t.trace_ids().iter().filter_map(|tid| {
                t.get(tid).map(|tr| (tid.clone(), tr.index()))
            }).collect()
        };

        let mut stopped = false;
        while !stopped {
            // Check stop condition
            if eval.eval_value_public(stop.clone())?.is_truthy() {
                break;
            }

            // Read current signal value
            let signal_val = {
                let t = traces.read().unwrap_or_else(|e| e.into_inner());
                t.first_trace().and_then(|tr| {
                    let idx = tr.index();
                    tr.signal_value(&signal_name, idx).ok()
                }).unwrap_or(ScalarValue::Bit(b'0'))
            };
            let val_value = scalar_to_value(signal_val);

            // Apply f(acc, signal_val) → new acc
            let call_expr = Value::List(WList::from_vec(vec![
                f.clone(),
                Value::List(WList::from_vec(vec![Value::Symbol(Symbol::new("quote")), acc])),
                Value::List(WList::from_vec(vec![Value::Symbol(Symbol::new("quote")), val_value])),
            ]));
            acc = eval.eval_value_public(call_expr)?;

            // Step forward
            let mut t = traces.write().unwrap_or_else(|e| e.into_inner());
            let mut any_ended = true;
            for trace in t.traces_iter_mut() {
                if trace.step(1).is_ok() { any_ended = false; }
            }
            stopped = any_ended;
        }

        // Restore positions
        let mut t = traces.write().unwrap_or_else(|e| e.into_inner());
        for (tid, idx) in &saved_positions {
            let _ = t.set_index(tid, *idx);
        }
        return Ok(acc);
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

    // Fast path: simple condition → find_indices
    if let Some(result) = try_find_indices_simple(cond, usize::MAX, env) {
        match result {
            Ok(Value::List(lst)) => return Ok(Value::Int(lst.len() as i64)),
            Ok(other) => return Ok(other),
            Err(e) => return Err(e),
        }
    }

    // Fallback: step-by-step scan
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
    disp.register(Operator::Timeframe, op_timeframe);
}