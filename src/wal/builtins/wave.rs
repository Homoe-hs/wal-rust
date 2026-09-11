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

/// 在多 trace 中查找信号所在的 trace: 逐个 trace 解析短名/子串,
/// 找到即回调 (trace, 解析后全名)。全部未命中 → 给出带候选的清晰错误。
/// (op_get 早已跨 trace 查找; getwave/wave/at 曾只看第一条 trace。)
fn with_signal_trace<R>(
    env: &Environment,
    name: &str,
    f: impl FnOnce(&dyn Trace, &str) -> Result<R, String>,
) -> Result<R, String> {
    let traces = env.get_traces()
        .ok_or_else(|| "No waveform loaded".to_string())?;
    let guard = traces.read().unwrap_or_else(|e| e.into_inner());
    let ids = guard.trace_ids();
    let mut ambiguous: Option<String> = None;
    for tid in &ids {
        let tr = match guard.get(tid) { Some(tr) => tr, None => continue };
        match tr.resolve_name_strict(name) {
            Ok(resolved) => return f(tr, &resolved),
            Err(e) => {
                // 歧义(有多个候选)是可操作的错误 → 直接返回; 否则继续找别的 trace
                if ambiguous.is_none() && e.contains("ambiguous") {
                    ambiguous = Some(e);
                }
            }
        }
    }
    Err(ambiguous.unwrap_or_else(|| {
        format!("signal '{}' not found in any loaded trace.", name)
    }))
}

/// Resolve a signal name strictly (exact or UNIQUE substring; 有歧义即报错)。
/// 走 trace.resolve_name_strict(零分配), 不再每次拉整张信号表。
fn resolve_signal(tr: &dyn Trace, name: &str) -> Result<String, String> {
    tr.resolve_name_strict(name)
}

/// IEEE 1364-1995 §14.1.1.4 / 1800-2012 §21.2.1.4 value convention:
/// a value WITHOUT x/z folds to an int; WITH x/z it is returned as a
/// bit-string (single lowercase `x` when all bits are unknown, `z` when all
/// are high-impedance, otherwise per-bit lowercase x/z — so a full-x vector
/// is visibly `"x"` and a partial-x vector `"10x1"` instead of a silently
/// folded integer).
pub(crate) fn scalar_to_value_x_aware(sv: &ScalarValue) -> Value {
    match sv {
        ScalarValue::Bit(b) => match b {
            b'x' | b'X' => Value::String("x".to_string()),
            b'z' | b'Z' => Value::String("z".to_string()),
            _ => Value::Int(if *b == b'1' { 1 } else { 0 }),
        },
        ScalarValue::Vector(v) => {
            if v.iter().all(|b| matches!(b, b'x' | b'X')) {
                return Value::String("x".to_string());
            }
            if v.iter().all(|b| matches!(b, b'z' | b'Z')) {
                return Value::String("z".to_string());
            }
            if v.iter().any(|b| matches!(b, b'x' | b'X' | b'z' | b'Z')) {
                return Value::String(
                    v.iter().map(|&b| (b as char).to_ascii_lowercase()).collect(),
                );
            }
            let int_val = v.iter().fold(0i64, |acc, &b| {
                (acc << 1) | if b == b'1' { 1 } else { 0 }
            });
            Value::Int(int_val)
        }
        ScalarValue::Real(r) => Value::Float(*r),
    }
}

fn scalar_to_wal(sv: &ScalarValue) -> Value {
    scalar_to_value_x_aware(sv)
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
    with_signal_trace(env, &name, |tr, sig| {
        let points = tr.change_points(sig)?;
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
    with_signal_trace(env, &name, |tr, sig| {
        let points = windowed_changes(tr, sig, t0, t1)?;
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
    with_signal_trace(env, &name, |tr, sig| {
        let points = tr.change_points(sig)?;
        let timed: Vec<(u64, ScalarValue)> = points.iter()
            .map(|(idx, sv)| (tr.timestamp_at(*idx).unwrap_or(0), sv.clone()))
            .collect();
        // last change at or before target time
        match timed.iter().rev().find(|(t, _)| *t <= target) {
            Some((t, sv)) => Ok(Value::List(WList::from_vec(vec![
                Value::Int(*t as i64), scalar_to_wal(sv),
            ]))),
            None => {
                // target precedes the first change: report the INITIAL held
                // value at time 0 (the $dumpvars snapshot, else x) — the value
                // held from the start of the timeline (feedback round B5:
                // "at 恒 (0 0)"/first-change answers were both misleading).
                let init_sv = tr.signal_value(&sig, 0).unwrap_or_else(|_| ScalarValue::Bit(b'x'));
                Ok(Value::List(WList::from_vec(vec![
                    Value::Int(0), scalar_to_wal(&init_sv),
                ])))
            }
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
            // Search pattern within the signal bit string since last check.
            // One result per change point: a pattern may match at several bit
            // offsets of the same value — report the timestamp once (B3).
            for (i, &b) in bits.iter().enumerate() {
                window.push(b);
                if window.len() > pattern.len() {
                    window.remove(0);
                }
                if window.len() == pattern.len() && window.iter().zip(&pattern).all(|(a, b)| a == b) {
                    if out.last() != Some(&Value::Int(t as i64)) {
                        out.push(Value::Int(t as i64));
                    }
                }
                let _ = i;
            }
        }
    }
    Ok(Value::List(WList::from_vec(out)))
    })
}

fn op_assert_eq(args: &[Value], env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    if args.len() != 4 {
        return Err("(assert-eq \"sig\" t0 t1 val) expected".to_string());
    }
    let name = extract_string(&args[0])?;
    let t0 = extract_int(&args[1])? as u64;
    let t1 = extract_int(&args[2])? as u64;
    let expected = extract_int(&args[3])?;
    // with_first_trace 的闭包不能借用 eval → 先在闭包里落 flag, 出来再记账
    let mut failed = false;
    let out = with_first_trace(env, |tr| {
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
            failed = true;
            eprintln!("assert-eq {} [{},{}] == {} FAILED:", sig, t0, t1, expected);
            for v in violations.iter().take(20) {
                eprintln!("  {}", v);
            }
            Ok(Value::Bool(false))
        }
    });
    if failed {
        // CI 判据: 会话/脚本退出码非 0(返回值仍是 false, 脚本仍可分支)
        eval.note_test_failure();
    }
    out
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
    ("take", "(take n list) → first n elements; e.g. (take 5 (find-sig \"clk\")) or (take 5 SIGNALS)"),
    ("search", "(search \"sig\" \"101\" [t0 t1]) → timestamps where the bit pattern occurs"),
    ("assert-eq", "(assert-eq \"sig\" t0 t1 v) → true if the signal equals v throughout [t0,t1]"),
    ("period", "(period \"clk\") → average clock period in seconds"),
    ("freq", "(freq \"clk\") → clock frequency in Hz"),
    ("save", "(save \"out.csv\" \"sig\"...) → export time/value columns to CSV"),
    ("fmt-time", "(fmt-time t [\"clk\"]) → t formatted with the waveform's timescale (e.g. 1.06ms); with a clock signal: \"beat N (1.06ms)\""),
    ("doc", "(doc \"cmd\"|cmd) → one-line documentation for a command"),
    ("semantics", "四值语义(权威口径): docs/4-state-semantics.md — 索引模型/取值与显示/比较/真值/边沿/聚合一致性"),
    ("x-semantics", "x 不是 0: (= x 0) 为假, (!= x 0) 为真, x→1 算变化但不算上升沿, 索引 0 无前驱(除 $dumpvars 给确定初值)"),
    ("INDEX", "INDEX 是当前游标索引; (get s) 取该索引处的值, 不随 map/遍历位置变化; 按时间取值用 (at s T)"),
    // ---- 核心查询算子(内测反馈: 这些以前查不到文档) ----
    ("get", "(get \"sig\" [hi lo]) → 索引处取值; 含 x/z 时返回位串(如 \"x\"/\"00x1\"), 否则整数"),
    ("at", "(at \"sig\" t) → (时间 值): t 时刻最后写入的值; 首变化前返回 (0 初值)"),
    ("rising", "(rising \"sig\") → 该索引处是否 0→非零(含 x/z 不算)"),
    ("falling", "(falling \"sig\") → 该索引处是否非零→0"),
    ("changes", "(changes \"sig\") → 该索引处值是否变化(x/z 之间的变化不算)"),
    ("is-x", "(is-x \"sig\") → 该索引处值是否含 x"),
    ("is-z", "(is-z \"sig\") → 该索引处值是否含 z"),
    ("count", "(count cond [cond...]) → 满足条件的索引数; (count/step cond) 逐索引等价形式"),
    ("find", "(find cond [limit]) → 满足条件的索引列表; (find/step cond) 逐索引等价形式"),
    ("whenever", "(whenever cond body...) → 在每个满足条件的索引处执行 body"),
    ("=", "(= a b ...) → 相等比较; 含 x/z 时按位串比较(== 是同一算子的别名)"),
    ("==", "(== a b) → 与 (= a b) 相同"),
    ("!=", "(!= a b) → 不等比较"),
    ("&&", "(&& a b ...) → 逻辑与(短路)"),
    ("||", "(|| a b ...) → 逻辑或(短路)"),
    ("not", "(not x) → 逻辑非(等价于 !)"),
    ("div", "(div a b) → 整数除法(向零取整); / 是浮点除法"),
    ("mod", "(mod a b) 或 (% a b) → 取模"),
    ("%", "(% a b) → 与 (mod a b) 相同"),
    ("help", "(help) → 打印算子/命令总览"),
    ("sigs", "(sigs \"pat\" [n]) → 信号名列表(子命令, 也可用 find-sig)"),
    ("topsig", "(topsig [n]) → 变化最多的信号(子命令)"),
    // NOTE: keep the unit semantics visible in (help) as well.
    ("timescale", "Times are in the waveform's NATIVE unit: raw numbers from getwave/wave/at/edges are ps/ns/... per the file's $timescale (see the load summary). Only period/freq and fmt-time convert to seconds / human units."),
];

/// (timescale) → print the loaded waveform's time unit (e.g. "1ns"/"?").
fn op_timescale(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    if !args.is_empty() {
        return Err("(timescale) takes no arguments".to_string());
    }
    with_first_trace(env, |tr| {
        let exp = tr.timescale_exp();
        match exp {
            Some(e) => {
                // VCD 的 $timescale 是"乘数 + 单位"(如 10ps = 10^-11 s);
                // 这里把指数还原成可读形式, 而不是对非 1 倍数的指数显示 "?"。
                // 取 <= e 的 3 的倍数(向 -inf 取整; Rust 的 / 向零截断, 负指数要再减一档)
                let unit_step = if e % 3 == 0 { e } else { (e / 3 - 1) * 3 };
                let mult = 10i64.pow((e - unit_step) as u32);
                let unit = match unit_step {
                    0 => "s", -3 => "ms", -6 => "us", -9 => "ns", -12 => "ps", -15 => "fs",
                    _ => "?",
                };
                println!("timescale: {}{} (10^{} s)", mult, unit, e);
            }
            None => println!("timescale: ?"),
        }
        Ok(Value::Nil)
    })
}

fn op_doc(args: &[Value], _env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    if args.len() != 1 {
        return Err("(doc \"cmd\") expected".to_string());
    }
    let topic = match &args[0] {
        Value::String(s) => s.clone(),
        Value::Symbol(s) => s.name.clone(),
        _ => return Err("(doc \"cmd\") expected".to_string()),
    };
    print_doc(&topic);
    Ok(Value::Nil)
}

/// 已文档化的主题名(供 (help) 列表展示)
pub(crate) fn documented_topics() -> Vec<String> {
    let mut v: Vec<String> = DOCS.iter().map(|(n, _)| (*n).to_string()).collect();
    v.sort();
    v.dedup();
    v
}

/// 打印某主题的文档(供 `doc` 特殊形式与内置实现共用)。
pub(crate) fn print_doc(topic: &str) {
    if let Some((_, doc)) = DOCS.iter().find(|(n, _)| *n == topic) {
        println!("{}", doc);
    } else {
        let mut names: Vec<&str> = DOCS.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        println!("No docs for '{}'. Documented topics: {}", topic, names.join(", "));
    }
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
    disp.register(Operator::Timescale, op_timescale);
}
