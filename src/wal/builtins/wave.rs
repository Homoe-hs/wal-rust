//! Time-aware waveform queries.
//!
//! Unlike the index-based `find`/`count` family, these builtins work with
//! actual timestamps (in the file's native time unit) so scripts can reason
//! about absolute time:
//!   (getwave "sig")              → ((t0 v0) (t1 v1) ...) all change points
//!   (wave "sig" t0 t1)           → change points in [t0, t1] (with the value
//!                                  held just before t0 as the first entry)
//!   (at "sig" t)                 → value at time t (last change ≤ t)
//!   (count-rise "sig" [t0 t1])   → rising edges in window
//!   (count-fall "sig" [t0 t1])   → falling edges in window
//!   (count-edges "sig" [t0 t1])  → all changes in window
//!   (edges "sig" [t0 t1])        → list of change timestamps
//!   (find-sig "pat")             → signal names matching a substring
//!   (search "sig" "101" [t0 t1]) → timestamps where the bit pattern occurs
//!   (assert-eq "sig" t0 t1 v)    → true if the signal equals v throughout
//!   (period "clk")               → average clock period in seconds
//!   (freq "clk")                 → frequency in Hz
//!   (save "out.csv" "sig"...)    → export time/value pairs to CSV
//!   (doc "getwave")              → one-line docs for a command

use crate::trace::{Trace, ScalarValue};
use crate::wal::ast::{Operator, Value, WList};
use crate::wal::eval::{Dispatcher, Environment, Evaluator};

fn extract_string(v: &Value) -> Result<String, String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Symbol(s) => Ok(s.name.clone()),
        _ => Err("Expected string".to_string()),
    }
}

fn extract_int(v: &Value) -> Result<i64, String> {
    match v {
        Value::Int(n) => Ok(*n),
        _ => Err("Expected integer".to_string()),
    }
}

/// Run a closure against the first loaded trace.
fn with_first_trace<R>(env: &Environment, f: impl FnOnce(&dyn Trace) -> Result<R, String>) -> Result<R, String> {
    let traces = env.get_traces()
        .ok_or_else(|| "No waveform loaded".to_string())?;
    let guard = traces.read().unwrap_or_else(|e| e.into_inner());
    let tr = guard.first_trace().ok_or_else(|| "No waveform loaded".to_string())?;
    f(tr)
}

/// Resolve a signal name (exact or unique substring) against a trace.
fn resolve_signal(tr: &dyn Trace, name: &str) -> Result<String, String> {
    let sigs = tr.signals();
    if sigs.iter().any(|s| s == name) {
        return Ok(name.to_string());
    }
    let matches: Vec<&String> = sigs.iter().filter(|s| s.contains(name)).collect();
    if matches.len() == 1 {
        return Ok(matches[0].clone());
    }
    Err(format!(
        "Signal '{}' not found ({} candidates: {:?})",
        name,
        matches.len(),
        matches.iter().take(5).map(|s| s.as_str()).collect::<Vec<_>>()
    ))
}

fn scalar_to_wal(sv: &ScalarValue) -> Value {
    match sv {
        ScalarValue::Bit(b) => Value::Int(if *b == b'1' { 1 } else { 0 }),
        ScalarValue::Vector(v) => {
            let int_val = v.iter().fold(0i64, |acc, &b| (acc << 1) | if b == b'1' { 1 } else { 0 });
            Value::Int(int_val)
        }
        ScalarValue::Real(r) => Value::Float(*r),
    }
}

/// (t v) pair
fn tv_pair(t: u64, sv: &ScalarValue) -> Value {
    Value::List(WList::from_vec(vec![Value::Int(t as i64), scalar_to_wal(sv)]))
}

/// Parse optional [t0 t1] window args (times in the file's native unit).
fn parse_window(args: &[Value]) -> (Option<u64>, Option<u64>) {
    match args.len() {
        0 => (None, None),
        1 => (extract_int(&args[0]).ok().map(|v| v as u64), None),
        _ => (
            extract_int(&args[0]).ok().map(|v| v as u64),
            extract_int(&args[1]).ok().map(|v| v as u64),
        ),
    }
}

/// All change points as (time, value), plus the value held just before t0
/// when a window start is given.
fn windowed_changes(tr: &dyn Trace, sig: &str, t0: Option<u64>, t1: Option<u64>)
    -> Result<Vec<(u64, ScalarValue)>, String>
{
    let points = tr.change_points(sig)?;
    // (time, value) list
    let timed: Vec<(u64, ScalarValue)> = points.iter()
        .map(|(idx, sv)| (tr.timestamp_at(*idx).unwrap_or(0), sv.clone()))
        .collect();
    let mut out: Vec<(u64, ScalarValue)> = Vec::new();
    // Value held just before t0 (the state entering the window)
    if let Some(t0) = t0 {
        if let Some(&(pt, ref pv)) = timed.iter().rev().find(|(t, _)| *t < t0) {
            if !timed.iter().any(|(t, _)| *t == t0) {
                out.push((pt, pv.clone()));
            }
        }
    }
    for (t, sv) in timed {
        let in_window = match (t0, t1) {
            (Some(a), Some(b)) => t >= a && t <= b,
            (Some(a), None) => t >= a,
            (None, Some(b)) => t <= b,
            (None, None) => true,
        };
        if in_window {
            out.push((t, sv));
        }
    }
    Ok(out)
}

fn op_getwave(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    if args.len() != 1 {
        return Err("(getwave \"sig\") expected".to_string());
    }
    let name = extract_string(&args[0])?;
    with_first_trace(env, |tr| {
        let sig = resolve_signal(tr, &name)?;
        let points = tr.change_points(&sig)?;
        let mut out = Vec::with_capacity(points.len());
        for (idx, sv) in points {
            let t = tr.timestamp_at(idx).unwrap_or(0);
            out.push(tv_pair(t, &sv));
        }
        Ok(Value::List(WList::from_vec(out)))
    })
}

fn op_wave(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    if args.len() < 1 || args.len() > 3 {
        return Err("(wave \"sig\" [t0 t1]) expected".to_string());
    }
    let name = extract_string(&args[0])?;
    let (t0, t1) = parse_window(&args[1..]);
    with_first_trace(env, |tr| {
        let sig = resolve_signal(tr, &name)?;
        let points = windowed_changes(tr, &sig, t0, t1)?;
        let mut out = Vec::with_capacity(points.len());
        for (t, sv) in points {
            out.push(tv_pair(t, &sv));
        }
        Ok(Value::List(WList::from_vec(out)))
    })
}

fn op_at(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    if args.len() != 2 {
        return Err("(at \"sig\" t) expected".to_string());
    }
    let name = extract_string(&args[0])?;
    let target = extract_int(&args[1])? as u64;
    with_first_trace(env, |tr| {
        let sig = resolve_signal(tr, &name)?;
        let points = tr.change_points(&sig)?;
        let timed: Vec<(u64, ScalarValue)> = points.iter()
            .map(|(idx, sv)| (tr.timestamp_at(*idx).unwrap_or(0), sv.clone()))
            .collect();
        // last change at or before target time
        match timed.iter().rev().find(|(t, _)| *t <= target) {
            Some((t, sv)) => Ok(Value::List(WList::from_vec(vec![
                Value::Int(*t as i64), scalar_to_wal(sv),
            ]))),
            None => Ok(Value::List(WList::from_vec(vec![Value::Int(0), Value::Int(0)]))),
        }
    })
}

fn edge_count(args: &[Value], env: &mut Environment, rising: bool) -> Result<Value, String> {
    if args.len() < 1 || args.len() > 3 {
        return Err("expected (count-rise \"sig\" [t0 t1])".to_string());
    }
    let name = extract_string(&args[0])?;
    let (t0, t1) = parse_window(&args[1..]);
    with_first_trace(env, |tr| {
        let sig = resolve_signal(tr, &name)?;
        let points = tr.change_points(&sig)?;
        let mut count = 0usize;
        let mut prev: Option<u8> = None;
        for (idx, sv) in &points {
            let bit = match sv {
                ScalarValue::Bit(b) => Some(*b),
                ScalarValue::Vector(v) if v.len() == 1 => Some(v[0]),
                _ => None,
            };
            if let (Some(p), Some(c)) = (prev, bit) {
                let is_rise = p == b'0' && c == b'1';
                let is_fall = p == b'1' && c == b'0';
                if (rising && is_rise) || (!rising && is_fall) {
                    let t = tr.timestamp_at(*idx).unwrap_or(0);
                    let in_window = match (t0, t1) {
                        (Some(a), Some(b)) => t >= a && t <= b,
                        (Some(a), None) => t >= a,
                        (None, Some(b)) => t <= b,
                        (None, None) => true,
                    };
                    if in_window { count += 1; }
                }
            }
            if let Some(b) = bit { prev = Some(b); }
        }
        Ok(Value::Int(count as i64))
    })
}

fn op_count_rise(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    edge_count(args, env, true)
}

fn op_count_fall(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    edge_count(args, env, false)
}

fn op_edges(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    if args.len() < 1 || args.len() > 3 {
        return Err("(edges \"sig\" [t0 t1]) expected".to_string());
    }
    let name = extract_string(&args[0])?;
    let (t0, t1) = parse_window(&args[1..]);
    with_first_trace(env, |tr| {
        let sig = resolve_signal(tr, &name)?;
        let points = tr.change_points(&sig)?;
        let mut out = Vec::new();
        for (i, (idx, _)) in points.iter().enumerate() {
            let t = tr.timestamp_at(*idx).unwrap_or(0);
            // skip the held-state point only when it lies before the window
            if i == 0 && t0.is_some() && t < t0.unwrap() { continue; }
            let in_window = match (t0, t1) {
                (Some(a), Some(b)) => t >= a && t <= b,
                (Some(a), None) => t >= a,
                (None, Some(b)) => t <= b,
                (None, None) => true,
            };
            if in_window {
                out.push(Value::Int(t as i64));
            }
        }
        Ok(Value::List(WList::from_vec(out)))
    })
}

fn op_count_edges(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    let edges = op_edges(args, env, _eval)?;
    if let Value::List(l) = edges {
        return Ok(Value::Int(l.0.len() as i64));
    }
    Ok(Value::Int(0))
}

fn op_find_sig(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    if args.len() != 1 {
        return Err("(find-sig \"pattern\") expected".to_string());
    }
    let pat = extract_string(&args[0])?;
    with_first_trace(env, |tr| {
        let mut out: Vec<Value> = tr.signals().into_iter()
            .filter(|s| s.contains(&pat))
            .map(Value::String)
            .collect();
        out.sort_by(|a, b| a.to_string().cmp(&b.to_string()));
        Ok(Value::List(WList::from_vec(out)))
    })
}

fn op_search(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    if args.len() < 2 || args.len() > 4 {
        return Err("(search \"sig\" \"101\" [t0 t1]) expected".to_string());
    }
    let name = extract_string(&args[0])?;
    let pattern = extract_string(&args[1])?;
    let (t0, t1) = parse_window(&args[2..]);
    with_first_trace(env, |tr| {
        let sig = resolve_signal(tr, &name)?;
        let points = tr.change_points(&sig)?;
    let pattern: Vec<char> = pattern.chars().collect();
    let mut out = Vec::new();
    let mut window: Vec<char> = Vec::new();
    for (idx, sv) in &points {
        let bits: Vec<char> = match sv {
            ScalarValue::Vector(v) => v.iter().map(|&b| b as char).collect(),
            ScalarValue::Bit(b) => vec![*b as char],
            _ => vec![],
        };
        if bits.is_empty() { continue; }
        let t = tr.timestamp_at(*idx).unwrap_or(0);
        let in_window = match (t0, t1) {
            (Some(a), Some(b)) => t >= a && t <= b,
            (Some(a), None) => t >= a,
            (None, Some(b)) => t <= b,
            (None, None) => true,
        };
        if in_window {
            // Search pattern within the signal bit string since last check
            for (i, &b) in bits.iter().enumerate() {
                window.push(b);
                if window.len() > pattern.len() {
                    window.remove(0);
                }
                if window.len() == pattern.len() && window.iter().zip(&pattern).all(|(a, b)| a == b) {
                    out.push(Value::Int(t as i64));
                }
                let _ = i;
            }
        }
    }
    Ok(Value::List(WList::from_vec(out)))
    })
}

fn op_assert_eq(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    if args.len() != 4 {
        return Err("(assert-eq \"sig\" t0 t1 val) expected".to_string());
    }
    let name = extract_string(&args[0])?;
    let t0 = extract_int(&args[1])? as u64;
    let t1 = extract_int(&args[2])? as u64;
    let expected = extract_int(&args[3])?;
    with_first_trace(env, |tr| {
        let sig = resolve_signal(tr, &name)?;
        let points = tr.change_points(&sig)?;
        let mut violations = Vec::new();
        for (idx, sv) in &points {
            let t = tr.timestamp_at(*idx).unwrap_or(0);
            if t < t0 { continue; }
            if t > t1 { break; }
            let actual = match scalar_to_wal(sv) {
                Value::Int(n) => n,
                other => { violations.push(format!("@{}: non-int {:?}", t, other)); continue; }
            };
            if actual != expected {
                violations.push(format!("@{}: got {}, want {}", t, actual, expected));
            }
        }
        if violations.is_empty() {
            Ok(Value::Bool(true))
        } else {
            eprintln!("assert-eq {} [{},{}] == {} FAILED:", sig, t0, t1, expected);
            for v in violations.iter().take(20) {
                eprintln!("  {}", v);
            }
            Ok(Value::Bool(false))
        }
    })
}

/// Average rising-edge interval of a clock signal in the waveform's native
/// time units. Returns None when there are not enough edges.
fn avg_rise_period_native(tr: &dyn Trace, sig: &str) -> Result<Option<f64>, String> {
    let points = tr.change_points(sig)?;
    let mut prev_rise: Option<u64> = None;
    let mut total = 0u64;
    let mut n = 0u64;
    for (idx, sv) in &points {
        let is_rise = match sv {
            ScalarValue::Bit(b) => *b == b'1',
            ScalarValue::Vector(v) if v.len() == 1 => v[0] == b'1',
            _ => continue,
        };
        if is_rise {
            let t = tr.timestamp_at(*idx).unwrap_or(0);
            if let Some(p) = prev_rise {
                total += t - p;
                n += 1;
            }
            prev_rise = Some(t);
        }
    }
    if n == 0 { Ok(None) } else { Ok(Some(total as f64 / n as f64)) }
}

fn op_period(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    if args.len() < 1 || args.len() > 2 {
        return Err("(period \"clk\") expected".to_string());
    }
    let name = extract_string(&args[0])?;
    with_first_trace(env, |tr| {
        let sig = resolve_signal(tr, &name)?;
        let period_native = avg_rise_period_native(tr, &sig)?
            .ok_or_else(|| format!("period: not enough rising edges for {}", sig))?;
        let exp = tr.timescale_exp().unwrap_or(-9) as i64;
        let scale = 10f64.powi(exp as i32);
        Ok(Value::Float(period_native * scale))
    })
}

fn op_freq(args: &[Value], env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    match op_period(args, env, eval)? {
        Value::Float(p) if p > 0.0 => Ok(Value::Float(1.0 / p)),
        Value::Float(_) => Err("freq: period is zero".to_string()),
        other => Err(format!("freq: unexpected period result {:?}", other)),
    }
}

fn op_save(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    if args.len() < 2 {
        return Err("(save \"out.csv\" \"sig1\" ...) expected".to_string());
    }
    let path = extract_string(&args[0])?;
    with_first_trace(env, |tr| {
        let mut header = String::new();
        header.push_str("time");
        let mut names: Vec<String> = Vec::new();
        for a in &args[1..] {
            let name = extract_string(a)?;
            let sig = resolve_signal(tr, &name)?;
            header.push(',');
            header.push_str(&sig);
            names.push(sig);
        }
        header.push('\n');
        // Union of all change timestamps
        let mut all: Vec<(u64, Vec<Option<ScalarValue>>)> = Vec::new();
        for (ci, sig) in names.iter().enumerate() {
            if let Ok(points) = tr.change_points(sig) {
                for (idx, sv) in points {
                    let t = tr.timestamp_at(idx).unwrap_or(0);
                    if let Some(entry) = all.iter_mut().find(|(t0, _)| *t0 == t) {
                        entry.1[ci] = Some(sv);
                    } else {
                        let mut vals: Vec<Option<ScalarValue>> = vec![None; names.len()];
                        vals[ci] = Some(sv);
                        all.push((t, vals));
                    }
                }
            }
        }
        all.sort_by_key(|(t, _)| *t);
        let mut body = String::new();
        for (t, vals) in all {
            body.push_str(&t.to_string());
            for v in vals {
                body.push(',');
                match v {
                    Some(sv) => body.push_str(&scalar_display(&sv)),
                    None => {}
                }
            }
            body.push('\n');
        }
        std::fs::write(&path, header + &body)
            .map_err(|e| format!("save: cannot write {}: {}", path, e))?;
        Ok(Value::Bool(true))
    })
}

fn scalar_display(sv: &ScalarValue) -> String {
    match sv {
        ScalarValue::Bit(b) => (*b as char).to_string(),
        ScalarValue::Vector(v) => v.iter().map(|&b| b as char).collect(),
        ScalarValue::Real(r) => format!("{}", r),
    }
}

const DOCS: &[(&str, &str)] = &[
    ("getwave", "(getwave \"sig\") → ((t v) ...) all change points with timestamps"),
    ("wave", "(wave \"sig\" t0 t1) → change points in [t0,t1] (held value before t0 first)"),
    ("at", "(at \"sig\" t) → value at time t (last change ≤ t)"),
    ("count-rise", "(count-rise \"sig\" [t0 t1]) → rising edges in window"),
    ("count-fall", "(count-fall \"sig\" [t0 t1]) → falling edges in window"),
    ("count-edges", "(count-edges \"sig\" [t0 t1]) → number of changes in window"),
    ("edges", "(edges \"sig\" [t0 t1]) → change timestamps in window"),
    ("find-sig", "(find-sig \"pattern\") → signal names containing the substring"),
    ("search", "(search \"sig\" \"101\" [t0 t1]) → timestamps where the bit pattern occurs"),
    ("assert-eq", "(assert-eq \"sig\" t0 t1 v) → true if the signal equals v throughout [t0,t1]"),
    ("period", "(period \"clk\") → average clock period in seconds"),
    ("freq", "(freq \"clk\") → clock frequency in Hz"),
    ("save", "(save \"out.csv\" \"sig\"...) → export time/value columns to CSV"),
    ("fmt-time", "(fmt-time t [\"clk\"]) → t formatted with the waveform's timescale (e.g. 1.06ms); with a clock signal: \"beat N (1.06ms)\""),
    ("doc", "(doc \"cmd\") → one-line documentation for a command"),
    // NOTE: keep the unit semantics visible in (help) as well.
    ("timescale", "Times are in the waveform's NATIVE unit: raw numbers from getwave/wave/at/edges are ps/ns/... per the file's $timescale (see the load summary). Only period/freq and fmt-time convert to seconds / human units."),
];

fn op_doc(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    if args.len() != 1 {
        return Err("(doc \"cmd\") expected".to_string());
    }
    let topic = extract_string(&args[0])?;
    if let Some((_, doc)) = DOCS.iter().find(|(n, _)| *n == topic) {
        println!("{}", doc);
    } else {
        println!("No docs for '{}'. Try (help) for the operator list.", topic);
    }
    Ok(Value::Nil)
}

/// Format a raw timestamp (native time units) into a human-readable string
/// using the waveform's timescale, e.g. 1060000ps -> "1.06ms".
fn format_timestamp(t: u64, exp: Option<i8>) -> String {
    let exp = exp.unwrap_or(-12) as i64;
    // seconds = t * 10^exp
    let seconds = t as f64 * 10f64.powi(exp as i32);
    const UNITS: &[(&str, f64)] = &[
        ("s", 1e0), ("ms", 1e-3), ("us", 1e-6), ("ns", 1e-9), ("ps", 1e-12), ("fs", 1e-15),
    ];
    for (name, scale) in UNITS {
        if seconds.abs() >= *scale {
            let v = seconds / scale;
            if v.abs() >= 1.0 || *name == "fs" {
                return format!("{:.2}{}", v, name);
            }
        }
    }
    format!("{:.2}s", seconds)
}

fn op_fmt_time(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    if args.len() < 1 || args.len() > 2 {
        return Err("(fmt-time t [\"clk\"]) expected".to_string());
    }
    let t = extract_int(&args[0])? as u64;
    with_first_trace(env, |tr| {
        let exp = tr.timescale_exp();
        // Beat mode: (fmt-time t "clk") → "beat N (human time)"
        if let Some(clk_arg) = args.get(1) {
            let clk = extract_string(clk_arg)?;
            let sig = resolve_signal(tr, &clk)?;
            if let Some(period) = avg_rise_period_native(tr, &sig)? {
                let beats = (t as f64 / period).round() as i64;
                return Ok(Value::String(format!(
                    "beat {} ({})",
                    beats,
                    format_timestamp(t, exp)
                )));
            }
        }
        Ok(Value::String(format_timestamp(t, exp)))
    })
}

/// List every operator with a one-line usage hint (used by (help) and startup).
pub fn operator_help_lines() -> Vec<String> {
    let mut lines = Vec::new();
    for (name, doc) in DOCS {
        lines.push(format!("  {:<14} {}", name, doc));
    }
    lines
}

pub fn register_wave(disp: &mut Dispatcher) {
    disp.register(Operator::GetWave, op_getwave);
    disp.register(Operator::Wave, op_wave);
    disp.register(Operator::At, op_at);
    disp.register(Operator::CountRise, op_count_rise);
    disp.register(Operator::CountFall, op_count_fall);
    disp.register(Operator::CountEdges, op_count_edges);
    disp.register(Operator::Edges, op_edges);
    disp.register(Operator::FindSig, op_find_sig);
    disp.register(Operator::Search, op_search);
    disp.register(Operator::AssertEq, op_assert_eq);
    disp.register(Operator::Period, op_period);
    disp.register(Operator::Freq, op_freq);
    disp.register(Operator::Save, op_save);
    disp.register(Operator::Doc, op_doc);
    disp.register(Operator::FmtTime, op_fmt_time);
}
