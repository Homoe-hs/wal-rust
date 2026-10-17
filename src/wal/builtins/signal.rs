//! Signal builtin operators
//!
//! load, unload, step, find, find/g, whenever, signal-width, sample-at, signal?

use crate::wal::ast::{Value, WList, Operator};
use crate::wal::eval::{Environment, Dispatcher, Evaluator, resolve_signal_name};
use crate::trace::{FindCondition, ResolveCompat, ScalarValue, Trace, TraceContainer};
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
        let tid2 = tid.clone();
        traces.load(Path::new(&path), tid)?;
        // Load summary: time range, signal count, timescale
        if let Some(tr) = traces.get(&tid2) {
            let n = tr.max_index();
            let t0 = tr.timestamp_at(0);
            let t1 = tr.timestamp_at(n);
            let unit = match tr.timescale_exp() {
                Some(0) => "s", Some(-3) => "ms", Some(-6) => "us",
                Some(-9) => "ns", Some(-12) => "ps", Some(-15) => "fs",
                _ => "?",
            };
            match (t0, t1) {
                (Some(a), Some(b)) => eprintln!(
                    "loaded {}: {} signals, {} timestamps, t=[{}{}, {}{}], timescale 10^{}s",
                    path, tr.signals().len(), n + 1, a, unit, b, unit,
                    tr.timescale_exp().map(|e| e.to_string()).unwrap_or_else(|| "?".into())
                ),
                _ => eprintln!(
                    "loaded {}: {} signals, timescale 10^{}s",
                    path, tr.signals().len(),
                    tr.timescale_exp().map(|e| e.to_string()).unwrap_or_else(|| "?".into())
                ),
            }
        }
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
    crate::wal::builtins::wave::scalar_to_value_x_aware(&sv)
}

#[allow(dead_code)]
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

/// Resolve symbol signal names inside a condition AST to their string
/// bindings, e.g. `(define s "tb.top.sig")` then `(count (= (get s) 1))`.
/// `count`/`find` are special forms and see the UNevaluated AST, so a bare
/// symbol `s` would otherwise miss the fast path (full per-step scan =
/// hang on big waves) or be matched as the literal name "s".
/// Ops with a signal-name argument: get, is-x, is-z, rising, falling, changes.
pub(crate) fn resolve_cond_names(expr: &Value, env: &Environment) -> Value {
    if let Value::List(lst) = expr {
        if lst.len() >= 2 {
            if let Value::Symbol(f) = &lst.0[0] {
                if matches!(
                    f.name.as_str(),
                    "get" | "is-x" | "is-z" | "rising" | "falling" | "changes"
                ) {
                    if let Value::Symbol(sym) = &lst.0[1] {
                        if let Some(val) = env.lookup(&sym.name) {
                            if matches!(val, Value::String(_)) {
                                let mut items = lst.0.clone();
                                items[1] = val.clone();
                                return Value::List(WList::from_vec(items));
                            }
                        }
                    }
                }
            }
        }
        let resolved: Vec<Value> = lst.0.iter().map(|v| resolve_cond_names(v, env)).collect();
        return Value::List(WList::from_vec(resolved));
    }
    expr.clone()
}

/// Try fast path for simple (= (get "sig") val) condition using find_indices
pub(crate) fn try_find_indices_simple(cond: &Value, max_results: usize, env: &mut Environment) -> Option<Result<Value, String>> {
    // Resolve symbol names (variable indirection) so the fast path applies
    // for e.g. (define s "sig") + (count (= (get s) 1)).
    let resolved = resolve_cond_names(cond, env);
    let cond = &resolved;
    // Edge/X conditions: (rising "sig"), (falling "sig"), (is-x "sig"),
    // (is-z "sig"), (changes "sig")
    if std::env::var("WAL_DEBUG_FIND").is_ok() {
        eprintln!("try_find_indices_simple: cond={:?}", cond);
    }
    if let Some((sig_name, cond_enum)) = parse_edge_condition(cond) {
        if std::env::var("WAL_DEBUG_FIND").is_ok() {
            eprintln!("  edge cond: {} {:?}", sig_name, cond_enum);
        }
        return try_find_indices_enum(&sig_name, cond_enum, max_results, env);
    }
    let (sig_name, target) = parse_simple_condition(cond)?;
    let cond_enum = if target <= 1 && target >= 0 {
        FindCondition::Value(target as u8)
    } else {
        FindCondition::ValueI64(target)
    };
    try_find_indices_enum(&sig_name, cond_enum, max_results, env)
}

/// `count` 专用: 只要匹配个数。默认后端会走 `find_indices(..).len()`; 支持
/// `count_matches` 的后端(如 FSDB 走 NPI)可以跳过"索引空间物化"。
pub(crate) fn try_count_simple(cond: &Value, env: &mut Environment) -> Option<Result<usize, String>> {
    let resolved = resolve_cond_names(cond, env);
    let (sig_name, cond_enum) = match parse_edge_condition(&resolved) {
        Some((n, c)) => (n, c),
        None => {
            let (n, target) = parse_simple_condition(&resolved)?;
            let c = if (0..=1).contains(&target) {
                FindCondition::Value(target as u8)
            } else {
                FindCondition::ValueI64(target)
            };
            (n, c)
        }
    };
    let traces = env.get_traces()?;
    // 选源: 第一条能解析出该名字的波形(第一条 -l 优先), 而不是无脑 first_trace()
    let n = {
        let t = traces.read().ok()?;
        let tid = t.source_ids(&[sig_name.clone()]).into_iter().next()?;
        let tr = t.get(&tid)?;
        let resolved = tr.resolve_name_compat(&sig_name)?;
        tr.count_matches(&resolved, cond_enum).ok()?
    };
    Some(Ok(n))
}

fn try_find_indices_enum(sig_name: &str, cond_enum: FindCondition, max_results: usize, env: &mut Environment) -> Option<Result<Value, String>> {

    let traces = env.get_traces()?;
    // 选源: 第一条能解析出该名字的波形(见 TraceContainer::find_indices)
    let (tid, resolved) = {
        let t = traces.read().ok()?;
        let tid = t.source_ids(&[sig_name.to_string()]).into_iter().next()?;
        let tr = t.get(&tid)?;
        let resolved = tr.resolve_name_compat(sig_name)?;
        (tid, resolved)
    };

    let indices = {
        let t = traces.read().ok()?;
        t.get(&tid)?.find_indices(&resolved, cond_enum).ok()?
    };

    let limited: Vec<Value> = indices.into_iter()
        .take(max_results)
        .map(|i| Value::Int(i as i64))
        .collect();

    Some(Ok(Value::List(WList::from_vec(limited))))
}

#[allow(dead_code)]
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

    // (whenever "changed" "sig" COND BODY...) — iterate only over timestamps
    // where "sig" changes value (edge-triggered sampling).
    let changed_mode = matches!(&args[0], Value::String(s) if s == "changed");
    let (cond, body) = if changed_mode {
        if args.len() < 3 {
            return Err("(whenever \"changed\" \"sig\" COND BODY...) expected".to_string());
        }
        (&args[2], &args[3..])
    } else {
        (&args[0], &args[1..])
    };

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

        // Changed-mode: iterate over the signal's change points only
        if changed_mode {
            let sig_name = match &args[1] {
                Value::String(s) => s.clone(),
                Value::Symbol(s) => s.name.clone(),
                _ => return Err("(whenever \"changed\" \"sig\" ...) signal name expected".to_string()),
            };
            if let Some(trace) = {
                let t = traces.read().unwrap_or_else(|e| e.into_inner());
                t.first_trace().map(|tr| (tr.id().clone(), tr.resolve_name_compat(&sig_name)))
            } {
                let (tid, resolved) = trace;
                if std::env::var("WAL_DEBUG_FIND").is_ok() {
                    eprintln!("whenever changed: sig={} resolved={:?}", sig_name, resolved);
                }
                if let Some(resolved) = resolved {
                    if let Ok(indices) = {
                        let t = traces.read().unwrap_or_else(|e| e.into_inner());
                        t.find_indices(&resolved, FindCondition::Changed)
                    } {
                        for &idx in &indices {
                            {
                                let mut t = traces.write().unwrap_or_else(|e| e.into_inner());
                                let _ = t.set_index(&tid, idx);
                            }
                            if eval.eval_value_public(cond.clone())?.is_truthy() {
                                for b in body {
                                    result = eval.eval_value_public(b.clone())?;
                                }
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
            let _ = cond;
            return Err(format!("whenever \"changed\": signal '{}' not found.", sig_name));
        }

        // Fast path: try simple condition (= (get "sig") val) → use find_indices
        if let Some((sig_name, target)) = parse_simple_condition(cond) {
            let cond_enum = if target <= 1 && target >= 0 {
                FindCondition::Value(target as u8)
            } else {
                FindCondition::ValueI64(target)
            };
            if let Some(trace) = {
                let t = traces.read().unwrap_or_else(|e| e.into_inner());
                t.first_trace().map(|tr| (tr.id().clone(), tr.resolve_name_compat(&sig_name)))
            } {
                let (tid, resolved) = trace;
                if let Some(resolved) = resolved {
                    if let Ok(indices) = {
                        let t = traces.read().unwrap_or_else(|e| e.into_inner());
                        t.find_indices(&resolved, cond_enum)
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

/// Parse a simple condition expression like (= (get "signal") N)
pub(crate) fn parse_edge_condition(expr: &Value) -> Option<(String, FindCondition)> {
    let lst = match expr {
        Value::List(lst) if lst.len() == 2 => lst,
        _ => return None,
    };
    let op = match &lst.0[0] {
        Value::Symbol(s) => s.name.as_str(),
        _ => return None,
    };
    let cond = match op {
        "rising" => FindCondition::Rising,
        "falling" => FindCondition::Falling,
        "is-x" => FindCondition::IsX,
        "is-z" => FindCondition::IsZ,
        "changes" => FindCondition::Changed,
        _ => return None,
    };
    let sig = match &lst.0[1] {
        Value::String(s) => s.clone(),
        Value::Symbol(s) => s.name.clone(),
        _ => return None,
    };
    Some((sig, cond))
}

pub(crate) fn parse_simple_condition(expr: &Value) -> Option<(String, i64)> {
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

fn op_get(args: &[Value], env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    // (get "sig")          full value at current INDEX
    // (get "sig" hi lo)    bit slice [hi:lo] inclusive (MSB hi, LSB lo)
    if args.len() != 1 && args.len() != 3 {
        return Err("(get \"sig\") or (get \"sig\" hi lo) expected".to_string());
    }
    let name = extract_name(&args[0])?;

    // 区间扫描覆盖(统一引擎): 边界处的当前值
    if let Some(cur) = env.sig_override_cur(&name) {
        let (hi, lo) = if args.len() == 3 {
            (extract_int(&args[1])? as u32, extract_int(&args[2])? as u32)
        } else {
            (1023, 0)
        };
        return Ok(slice_value(cur, hi, lo));
    }

    // Check virtual signals first
    if env.is_virtual_signal(&name) {
        if let Some(expr) = env.lookup(&name) {
            return eval.eval_value_public(expr);
        }
    }

    let (hi, lo) = if args.len() == 3 {
        let hi_v = extract_int(&args[1])?;
        let lo_v = extract_int(&args[2])?;
        if hi_v < lo_v || lo_v < 0 || hi_v > 1023 {
            return Err(format!("invalid bit slice [{}:{}]", hi_v, lo_v));
        }
        (hi_v as u32, lo_v as u32)
    } else {
        (1023, 0)
    };

    // Check virtual signals first
    if env.is_virtual_signal(&name) {
        if let Some(expr) = env.lookup(&name) {
            return eval.eval_value_public(expr);
        }
    }

    // Try exact candidates with scope/group prepended
    let mut candidates = vec![name.clone(), format!("{}{}", env.get_scope(), name)];
    candidates.extend(crate::wal::builtins::scope::group_prefixed_names(
        env.get_group(), &name,
    ));

    if let Some(traces) = env.get_traces() {
        let traces = traces.read().unwrap_or_else(|e| e.into_inner());
        // Search every loaded trace (not just the first): the same short name
        // may live in a second -l file (multi-trace get, feedback round B12).
        // 模糊匹配走 trace.resolve_name_compat(零分配): 此前每个 trace 都 signals() 拉整张表
        // (4M 信号 ≈ 0.6-0.8s/次)。候选提示只在最终失败时用 suggest_names 取前 5 个。
        let mut preview: Vec<String> = Vec::new();
        for trace in traces.traces_iter() {
            for candidate in &candidates {
                if let Ok(sv) = trace.signal_value(candidate, trace.index()) {
                    return Ok(slice_value(sv, hi, lo));
                }
            }
            if let Some(matched) = trace.resolve_name_compat(&name) {
                if let Ok(sv) = trace.signal_value(&matched, trace.index()) {
                    return Ok(slice_value(sv, hi, lo));
                }
            }
            if preview.is_empty() {
                preview = trace.suggest_names(&name, 5);
            }
        }
        if !preview.is_empty() {
            return Err(format!("signal '{}' not found. Closest signals: {:?}", name, preview));
        }
    }
    Err(format!("signal '{}' not found.", name))
}

/// Bit-slice a ScalarValue: extract bits [hi:lo] (inclusive, MSB=hi).
fn slice_value(sv: ScalarValue, hi: u32, lo: u32) -> Value {
    if hi == 63 && lo == 0 {
        return scalar_to_value(sv);
    }
    match sv {
        ScalarValue::Bit(b) => match b {
            // x/z 保持未知: 1bit 与向量口径一致(返回位串而不是 0)
            b'x' | b'X' => Value::String("x".to_string()),
            b'z' | b'Z' => Value::String("z".to_string()),
            _ if lo == 0 => Value::Int(if b == b'1' { 1 } else { 0 }),
            _ => Value::Int(0),
        },
        ScalarValue::Vector(v) => {
            // v[0] is the MSB of the signal (same convention as wal's to_int)
            let width = v.len() as u32;
            if lo >= width {
                return Value::Int(0);
            }
            let h = hi.min(width - 1);
            let mut bits: Vec<u8> = Vec::with_capacity((h - lo + 1) as usize);
            for bit in (lo..=h).rev() {
                let idx = (width - 1 - bit) as usize;
                let b = *v.get(idx).unwrap_or(&b'0');
                bits.push(b);
            }
            crate::wal::builtins::wave::scalar_to_value_x_aware(&ScalarValue::Vector(bits))
        }
        ScalarValue::Real(r) => Value::Float(r),
    }
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

fn op_signal_width(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let name = extract_name(&args[0])?;
    if let Some(traces) = env.get_traces() {
        let traces_lock = traces.read().unwrap_or_else(|e| e.into_inner());
        if let Some(trace) = traces_lock.first_trace() {
            let resolved = trace.resolve_name_compat(&name).unwrap_or(name.clone());
            if let Ok(w) = trace.signal_width(&resolved) {
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
    let name = extract_name(&args[0])?;
    let signal_name = resolve_signal_name(&name, &[]).unwrap_or(name);
    let _ = &signal_name;
    let index = match &args[1] {
        Value::Int(i) if *i >= 0 => *i as usize,
        Value::Int(i) => return Err(format!("sample-at: 索引不能为负: {}", i)),
        // 浮点索引**不能静默截断**: `(sample-at s 4.5)` 曾经取到索引 4 的值,
        // 于是"按时间/按拍取值"写错的人会拿到一个看起来正常的数(静默错值)。
        Value::Float(f) => {
            return Err(format!(
                "sample-at: 索引必须是整数, 收到 {}。要按索引取整可用 (div n 2)",
                f
            ))
        }
        _ => return Err("sample-at: second argument must be an integer index".to_string()),
    };
    if let Some(traces) = env.get_traces() {
        let traces_lock = traces.read().unwrap_or_else(|e| e.into_inner());
        if let Some(trace) = traces_lock.first_trace() {
            let resolved = trace.resolve_name_compat(&signal_name).unwrap_or(signal_name.clone());
            if let Ok(sv) = trace.signal_value(&resolved, index) {
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

// ---- waveform-debug condition builtins -----------------------------------
// (rising "sig") (falling "sig") (is-x "sig") (is-z "sig") (changes "sig")
// Used inside count/find/whenever conditions; evaluate at the current INDEX.

#[allow(dead_code)] // 参考实现: 逐拍风格的单信号边沿取值(现走 parse_edge_condition)
fn edge_value(name: &str, env: &mut Environment) -> Result<Option<(Option<u8>, Option<u8>)>, String> {
    if let Some(traces) = env.get_traces() {
        let traces = traces.read().unwrap_or_else(|e| e.into_inner());
        if let Some(trace) = traces.first_trace() {
            let idx = trace.index();
            if idx == 0 {
                return Ok(None);
            }
            let cur = trace.signal_value(name, idx).ok();
            let prev = trace.signal_value(name, idx - 1).ok();
            let cur_bit = cur.as_ref().and_then(|v| match v {
                ScalarValue::Bit(b) => Some(*b),
                ScalarValue::Vector(v) if v.len() == 1 => Some(v[0]),
                _ => None,
            });
            let prev_bit = prev.as_ref().and_then(|v| match v {
                ScalarValue::Bit(b) => Some(*b),
                ScalarValue::Vector(v) if v.len() == 1 => Some(v[0]),
                _ => None,
            });
            return Ok(Some((cur_bit, prev_bit)));
        }
    }
    Ok(None)
}

fn scalar_is_zero(sv: Option<&ScalarValue>) -> Option<bool> {
    let sv = sv?;
    match sv {
        ScalarValue::Bit(b) => match *b {
            b'0' => Some(true),
            b'1' => Some(false),
            _ => None,
        },
        ScalarValue::Vector(v) => {
            if v.iter().all(|b| *b == b'0' || *b == b'1') {
                Some(v.iter().all(|b| *b == b'0'))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn scalar_has_x(v: &ScalarValue) -> bool {
    match v {
        ScalarValue::Bit(b) => *b == b'x' || *b == b'X',
        ScalarValue::Vector(vec) => vec.iter().any(|&b| b == b'x' || b == b'X'),
        _ => false,
    }
}

fn scalar_has_z(v: &ScalarValue) -> bool {
    match v {
        ScalarValue::Bit(b) => *b == b'z' || *b == b'Z',
        ScalarValue::Vector(vec) => vec.iter().any(|&b| b == b'z' || b == b'Z'),
        _ => false,
    }
}

/// 边沿判定(与逐拍路径同一语义): 向量按"零/非零",位按 0↔1;x 不算。
fn edge_rising(prev: Option<&ScalarValue>, cur: Option<&ScalarValue>) -> bool {
    match (scalar_is_zero(prev), scalar_is_zero(cur)) {
        (Some(true), Some(false)) => return true,
        _ => {}
    }
    matches!((prev, cur),
        (Some(ScalarValue::Bit(b'0')), Some(ScalarValue::Bit(b'1'))))
}

fn edge_falling(prev: Option<&ScalarValue>, cur: Option<&ScalarValue>) -> bool {
    match (scalar_is_zero(prev), scalar_is_zero(cur)) {
        (Some(false), Some(true)) => return true,
        _ => {}
    }
    matches!((prev, cur),
        (Some(ScalarValue::Bit(b'1')), Some(ScalarValue::Bit(b'0'))))
}

/// 跨 trace 定位信号并读取 (prev, cur):
/// - `Ok(None)`: 没有加载任何波形(与旧行为一致, 谓词为 false)
/// - `Ok(Some((prev, cur)))`: 找到信号; idx==0 时 prev = $dumpvars 确定初值(无则 None)
/// - `Err`: 所有 trace 都没有这个信号(拼写错误要报错, 不能静默 false)
fn edge_values_at_cursor(
    env: &Environment,
    name: &str,
) -> Result<Option<(Option<ScalarValue>, ScalarValue)>, String> {
    let traces = match env.get_traces() { Some(t) => t, None => return Ok(None) };
    let traces = traces.read().unwrap_or_else(|e| e.into_inner());
    let mut any_trace = false;
    for tr in traces.traces_iter() {
        any_trace = true;
        let resolved = match tr.resolve_name_compat(name) { Some(r) => r, None => continue };
        let idx = tr.index();
        let cur = match tr.signal_value(&resolved, idx) { Ok(v) => v, Err(_) => continue };
        let prev = if idx == 0 {
            tr.defined_initial_value(&resolved)
        } else {
            tr.signal_value(&resolved, idx - 1).ok()
        };
        return Ok(Some((prev, cur)));
    }
    if any_trace {
        Err(format!("signal '{}' not found in any loaded trace.", name))
    } else {
        Ok(None)
    }
}

fn op_rising(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    if env.edge_off() { return Ok(Value::Bool(false)); }
    let name = extract_name(&args[0])?;
    // 区间扫描覆盖(统一引擎): 直接用边界处的 (prev, cur) 求值
    if let Some((prev, cur)) = env.sig_override_pair(&name) {
        return Ok(Value::Bool(edge_rising(prev.as_ref(), Some(&cur))));
    }
    if let Some((prev, cur)) = edge_values_at_cursor(env, &name)? {
        match (scalar_is_zero(prev.as_ref()), scalar_is_zero(Some(&cur))) {
            (Some(true), Some(false)) => return Ok(Value::Bool(true)),
            _ => {}
        }
        if let (ScalarValue::Bit(c), Some(ScalarValue::Bit(p))) = (&cur, &prev) {
            return Ok(Value::Bool(*c == b'1' && *p == b'0'));
        }
    }
    Ok(Value::Bool(false))
}

fn op_falling(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    if env.edge_off() { return Ok(Value::Bool(false)); }
    let name = extract_name(&args[0])?;
    if let Some((prev, cur)) = env.sig_override_pair(&name) {
        return Ok(Value::Bool(edge_falling(prev.as_ref(), Some(&cur))));
    }
    if let Some((prev, cur)) = edge_values_at_cursor(env, &name)? {
        match (scalar_is_zero(prev.as_ref()), scalar_is_zero(Some(&cur))) {
            (Some(false), Some(true)) => return Ok(Value::Bool(true)),
            _ => {}
        }
        if let (ScalarValue::Bit(c), Some(ScalarValue::Bit(p))) = (&cur, &prev) {
            return Ok(Value::Bool(*c == b'0' && *p == b'1'));
        }
    }
    Ok(Value::Bool(false))
}

fn op_is_x(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let name = extract_name(&args[0])?;
    if let Some(cur) = env.sig_override_cur(&name) {
        return Ok(Value::Bool(scalar_has_x(&cur)));
    }
    if let Some((_, cur)) = edge_values_at_cursor(env, &name)? {
        let has_x = match &cur {
            ScalarValue::Bit(b) => *b == b'x' || *b == b'X',
            ScalarValue::Vector(v) => v.iter().any(|b| *b == b'x' || *b == b'X'),
            _ => false,
        };
        return Ok(Value::Bool(has_x));
    }
    Ok(Value::Bool(false))
}

fn op_is_z(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let name = extract_name(&args[0])?;
    if let Some(cur) = env.sig_override_cur(&name) {
        return Ok(Value::Bool(scalar_has_z(&cur)));
    }
    if let Some((_, cur)) = edge_values_at_cursor(env, &name)? {
        let has_z = match &cur {
            ScalarValue::Bit(b) => *b == b'z' || *b == b'Z',
            ScalarValue::Vector(v) => v.iter().any(|b| *b == b'z' || *b == b'Z'),
            _ => false,
        };
        return Ok(Value::Bool(has_z));
    }
    Ok(Value::Bool(false))
}

fn op_changes(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    if env.edge_off() { return Ok(Value::Bool(false)); }
    let name = extract_name(&args[0])?;
    if let Some((prev, cur)) = env.sig_override_pair(&name) {
        return Ok(Value::Bool(match prev {
            Some(p) => p != cur,
            None => false,
        }));
    }
    if let Some((prev, cur)) = edge_values_at_cursor(env, &name)? {
        // whole-value comparison (vectors included); 索引 0 用 $dumpvars 确定初值
        if let Some(p) = prev {
            return Ok(Value::Bool(cur != p));
        }
        return Ok(Value::Bool(false));
    }
    Ok(Value::Bool(false))
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

fn op_eval_file(args: &[Value], _env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let path = extract_string(&args[0])?;
    let source = std::fs::read_to_string(&path)
        .map_err(|e| format!("eval-file: cannot read '{}': {}", path, e))?;
    eval.eval(&source)
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

#[allow(dead_code)]
fn op_count(args: &[Value], env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    ensure_arity(args, 1)?;
    let cond = &args[0];

    if std::env::var("WAL_DEBUG_FIND").is_ok() {
        eprintln!("op_count: cond={:?} edge={:?}", cond, parse_edge_condition(cond).map(|e| (e.0.clone(), e.1)));
    }

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
    disp.register(Operator::Help, crate::wal::builtins::core::help_dispatch);
    disp.register(Operator::Unload, op_unload);
    disp.register(Operator::Step, op_step);
    disp.register(Operator::Whenever, op_whenever);
        disp.register(Operator::SignalWidth, op_signal_width);
    disp.register(Operator::SampleAt, op_sample_at);
        disp.register(Operator::IsSignal, op_signal_p);
    disp.register(Operator::Get, op_get);
        disp.register(Operator::EvalFile, op_eval_file);
        disp.register(Operator::LoadedTraces, op_loaded_traces);
    disp.register(Operator::RelEval, op_releval);
    disp.register(Operator::Timeframe, op_timeframe);
    disp.register(Operator::Rising, op_rising);
    disp.register(Operator::Falling, op_falling);
    disp.register(Operator::IsX, op_is_x);
    disp.register(Operator::IsZ, op_is_z);
    disp.register(Operator::Changes, op_changes);
    disp.register(Operator::Signals, op_signals);
    disp.register(Operator::Index, op_index);
    disp.register(Operator::MaxIndex, op_max_index);
    disp.register(Operator::Ts, op_ts);
    disp.register(Operator::TraceName, op_trace_name);
    disp.register(Operator::TraceFile, op_trace_file);
}
/// Local Levenshtein distance (for "closest signal" hints on not-found).
pub(crate) fn lev_distance_local(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            cur.push((prev[j] + 1).min(cur[j] + 1).min(prev[j + 1] + cost));
        }
        prev = cur;
    }
    prev[b.len()]
}
