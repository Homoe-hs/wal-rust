//! 波形指标与统计(延迟/性能分布) —— 见 `docs/waveform-metrics.md`。
//!
//! 硬件团队要的是**分布**, 不是单个数字: "握手平均多少拍"没有意义,
//! "p50/p99/最坏多少"才有意义; 同理 IPC 不只看均值, 还要看随时间的波动。
//! 所以这一层把 中位数/分位数/标准差/直方图 做成一等算子, 而不是让用户自己写循环。
//!
//! 分两层, 都能单独用、也能串起来:
//!
//! * **纯统计**(`median`/`percentile`/`stddev`/`stats`/`histogram`)只吃数值列表 ——
//!   任何 `(find ...)`/`(getwave ...)` 的产物都能直接喂进去;
//! * **波形指标**(`latency`/`ipc`)从 trace 取变更时刻, 输出数值列表/数值,
//!   因此可以**直接组合**: `(stats (latency (rising "req") (rising "ack")))`。
//!
//! ## 语义约定(先定死, 免得各人各解)
//!
//! * 空列表: `median`/`percentile`/`stddev`/`stats` 返回 NaN 或 0 而不是报错 ——
//!   "这类事件一次都没发生"也要能跑完并打印 `n=0`;
//! * `percentile` 用**线性插值**(与 numpy 默认口径一致): `[1,2,3,4]` 的 p50 = 2.5;
//! * `stddev` 是**样本标准差**(n-1), n<2 时为 0;
//! * 一切以 f64 计算; `stats` 的 count 是整数;
//! * **`latency` 的单位是波形原生时间单位**(ps/ns/…, 与 `getwave`/`at` 一致),
//!   不是"拍数"。要拍数就除以时钟周期: `(/ d (period "clk"))`;
//! * **`latency` 默认按 FIFO 配对**(先到的 start 配下一个 end), 选项 `#restart`
//!   表示"新的 start 直接作废上一个还没配对的 start"(适合"重试/覆盖"语义);
//!   配不上的 start / end 直接丢弃(不编造负延迟), 想要个数就用
//!   `(count (rising "start"))` 与 `(len (latency ...))` 对比;
//! * **`ipc` = 事件数 / 时钟周期数**, 两者都只在**时间域**数(不走索引空间,
//!   所以在大波形上不会触发"全文件物化时间线")。

use crate::trace::{FindCondition, ScalarValue, Trace};
use crate::wal::ast::{Operator, Value, WList};
use crate::wal::eval::{Dispatcher, Environment, Evaluator};

// ============================ 纯统计 ============================

/// 把参数里的列表取成 f64 向量; 非数值元素报错(不静默跳过)
fn list_to_f64(v: &Value, who: &str) -> Result<Vec<f64>, String> {
    let list = match v {
        Value::List(l) => &l.0,
        // 允许标量当单元素序列: `(median 5)` → 5(脚本里省一层 (list ...))
        Value::Int(i) => return Ok(vec![*i as f64]),
        Value::Float(f) => return Ok(vec![*f]),
        _ => return Err(format!("{} expects a list of numbers", who)),
    };
    let mut out = Vec::with_capacity(list.len());
    for e in list {
        match e {
            Value::Int(i) => out.push(*i as f64),
            Value::Float(f) => out.push(*f),
            // 延迟列表里不该出现别的类型; 真出现了就说清楚是谁
            other => return Err(format!("{} expects numbers, got {}", who, other.type_name())),
        }
    }
    Ok(out)
}

/// 升序序列的线性插值分位数(与 numpy.percentile 默认口径一致)
fn percentile_sorted(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let p = p.clamp(0.0, 100.0);
    let rank = (p / 100.0) * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        return sorted[lo];
    }
    let frac = rank - lo as f64;
    sorted[lo] * (1.0 - frac) + sorted[hi] * frac
}

/// 分位数(会先排序一份副本)
fn percentile(values: &[f64], p: f64) -> f64 {
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    percentile_sorted(&v, p)
}

/// 样本标准差(n-1; n<2 → 0)
fn sample_stddev(values: &[f64]) -> f64 {
    let n = values.len();
    if n < 2 {
        return 0.0;
    }
    let mean = values.iter().sum::<f64>() / n as f64;
    let var = values.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / (n - 1) as f64;
    var.sqrt()
}

fn op_median(args: &[Value], _env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    if args.len() != 1 {
        return Err("(median xs) expected".to_string());
    }
    let v = eval.eval_value_public(args[0].clone())?;
    let xs = list_to_f64(&v, "median")?;
    Ok(Value::Float(percentile(&xs, 50.0)))
}

fn op_percentile(
    args: &[Value],
    _env: &mut Environment,
    eval: &mut Evaluator,
) -> Result<Value, String> {
    if args.len() != 2 {
        return Err("(percentile xs p) expected — p in 0..100".to_string());
    }
    let v = eval.eval_value_public(args[0].clone())?;
    let p = match eval.eval_value_public(args[1].clone())? {
        Value::Int(i) => i as f64,
        Value::Float(f) => f,
        _ => return Err("percentile: p must be a number (0..100)".to_string()),
    };
    if !(0.0..=100.0).contains(&p) {
        return Err(format!("percentile: p must be in 0..100, got {}", p));
    }
    let xs = list_to_f64(&v, "percentile")?;
    Ok(Value::Float(percentile(&xs, p)))
}

fn op_stddev(args: &[Value], _env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    if args.len() != 1 {
        return Err("(stddev xs) expected".to_string());
    }
    let v = eval.eval_value_public(args[0].clone())?;
    let xs = list_to_f64(&v, "stddev")?;
    Ok(Value::Float(sample_stddev(&xs)))
}

fn fmt_num(x: f64) -> String {
    if x.is_nan() {
        return "n/a".to_string();
    }
    if x.fract() == 0.0 && x.abs() < 1e15 {
        return format!("{}", x as i64);
    }
    format!("{:.3}", x)
}

/// `(stats xs)` → 打印一行摘要, 返回 `(count min p50 mean p90 p99 max stddev)`
fn op_stats(args: &[Value], _env: &mut Environment, eval: &mut Evaluator) -> Result<Value, String> {
    if args.len() != 1 {
        return Err("(stats xs) expected".to_string());
    }
    let v = eval.eval_value_public(args[0].clone())?;
    let xs = list_to_f64(&v, "stats")?;
    Ok(stats_value(&xs))
}

/// 统计摘要(打印一行 + 返回定长列表); 空列表也照常返回, 不报错
fn stats_value(xs: &[f64]) -> Value {
    let n = xs.len();
    let (mn, mx, mean) = if n == 0 {
        (f64::NAN, f64::NAN, f64::NAN)
    } else {
        let mn = xs.iter().cloned().fold(f64::INFINITY, f64::min);
        let mx = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        (mn, mx, xs.iter().sum::<f64>() / n as f64)
    };
    let p50 = percentile(xs, 50.0);
    let p90 = percentile(xs, 90.0);
    let p99 = percentile(xs, 99.0);
    let sd = sample_stddev(xs);
    println!(
        "n={} min={} p50={} mean={} p90={} p99={} max={} stddev={}",
        n,
        fmt_num(mn),
        fmt_num(p50),
        fmt_num(mean),
        fmt_num(p90),
        fmt_num(p99),
        fmt_num(mx),
        fmt_num(sd)
    );
    Value::List(WList::from_vec(vec![
        Value::Int(n as i64),
        Value::Float(mn),
        Value::Float(p50),
        Value::Float(mean),
        Value::Float(p90),
        Value::Float(p99),
        Value::Float(mx),
        Value::Float(sd),
    ]))
}

/// `(histogram xs [bins])` → 打印条图, 返回 `(lo hi count ...)` 的扁平列表(边界 ×1000)
fn op_histogram(
    args: &[Value],
    _env: &mut Environment,
    eval: &mut Evaluator,
) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err("(histogram xs [bins]) expected".to_string());
    }
    let v = eval.eval_value_public(args[0].clone())?;
    let xs = list_to_f64(&v, "histogram")?;
    let bins = if args.len() == 2 {
        match eval.eval_value_public(args[1].clone())? {
            Value::Int(i) if i > 0 => i as usize,
            _ => return Err("histogram: bins must be a positive integer".to_string()),
        }
    } else {
        10
    };
    Ok(histogram_value(&xs, bins))
}

fn histogram_value(xs: &[f64], bins: usize) -> Value {
    if xs.is_empty() {
        println!("(empty)");
        return Value::List(WList::new());
    }
    let mn = xs.iter().cloned().fold(f64::INFINITY, f64::min);
    let mx = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let span = if mx > mn { mx - mn } else { 1.0 };
    let width = span / bins as f64;
    let mut counts = vec![0usize; bins];
    for x in xs {
        let mut b = ((x - mn) / width).floor() as i64;
        if b < 0 {
            b = 0;
        }
        if b as usize >= bins {
            b = bins as i64 - 1; // 最大值归入最后一桶
        }
        counts[b as usize] += 1;
    }
    let peak = *counts.iter().max().unwrap_or(&1);
    let mut out: Vec<Value> = Vec::with_capacity(bins * 3);
    for (i, c) in counts.iter().enumerate() {
        let lo = mn + width * i as f64;
        let hi = mn + width * (i + 1) as f64;
        let bar_len = if peak == 0 { 0 } else { (c * 40 / peak).max(if *c > 0 { 1 } else { 0 }) };
        println!("{:>12.3} .. {:<12.3} {:>8} |{}", lo, hi, c, "#".repeat(bar_len));
        // 桶边界保留 3 位小数(输出是给人读/贴报告的)
        out.push(Value::Int((lo * 1000.0).round() as i64));
        out.push(Value::Int((hi * 1000.0).round() as i64));
        out.push(Value::Int(*c as i64));
    }
    Value::List(WList::from_vec(out))
}

// ============================ 波形指标 ============================

fn bit_of(v: &ScalarValue) -> Option<u8> {
    match v {
        ScalarValue::Bit(b) => Some(*b),
        ScalarValue::Vector(bytes) if bytes.len() == 1 => Some(bytes[0]),
        _ => None,
    }
}

/// 位向量 → 整数(FSDB/VCD 的 `Vector` 按 LSB 在前存放)
fn vector_to_i64(bytes: &[u8]) -> i64 {
    let mut v: i64 = 0;
    for (i, b) in bytes.iter().enumerate() {
        if i >= 63 {
            break;
        }
        if *b == b'1' {
            v |= 1 << i;
        }
    }
    v
}

fn value_is(v: &ScalarValue, target: i64) -> bool {
    match v {
        ScalarValue::Bit(b) => (*b as i64) == target,
        ScalarValue::Vector(bytes) => vector_to_i64(bytes) == target,
        ScalarValue::Real(f) => *f == target as f64,
    }
}

/// 这一条变更算不算条件命中?(前驱口径与查询引擎一致: 边沿看"前一个值")
fn edge_matches(prev: Option<&ScalarValue>, cur: &ScalarValue, cond: &FindCondition) -> bool {
    match cond {
        FindCondition::Rising => bit_of(prev.unwrap_or(&ScalarValue::Bit(b'x'))) == Some(b'0')
            && bit_of(cur) == Some(b'1'),
        FindCondition::Falling => bit_of(prev.unwrap_or(&ScalarValue::Bit(b'x'))) == Some(b'1')
            && bit_of(cur) == Some(b'0'),
        FindCondition::Changed => prev.map(|p| p != cur).unwrap_or(false),
        FindCondition::High => bit_of(cur) == Some(b'1'),
        FindCondition::Low => bit_of(cur) == Some(b'0'),
        FindCondition::Value(v) => {
            let target = *v as i64;
            !prev.map(|p| value_is(p, target)).unwrap_or(false) && value_is(cur, target)
        }
        FindCondition::ValueI64(v) => {
            !prev.map(|p| value_is(p, *v)).unwrap_or(false) && value_is(cur, *v)
        }
        FindCondition::Neq(v) => {
            prev.map(|p| value_is(p, *v as i64)).unwrap_or(false) && !value_is(cur, *v as i64)
        }
        FindCondition::NeqI64(v) => {
            prev.map(|p| value_is(p, *v)).unwrap_or(false) && !value_is(cur, *v)
        }
        FindCondition::IsX => {
            let now = matches!(cur, ScalarValue::Bit(b'x') | ScalarValue::Bit(b'z'))
                || matches!(cur, ScalarValue::Vector(v) if v.iter().any(|b| *b == b'x' || *b == b'z'));
            let was = prev
                .map(|p| {
                    matches!(p, ScalarValue::Bit(b'x') | ScalarValue::Bit(b'z'))
                        || matches!(p, ScalarValue::Vector(v) if v.iter().any(|b| *b == b'x' || *b == b'z'))
                })
                .unwrap_or(false);
            now && !was
        }
        FindCondition::IsZ => {
            let now = matches!(cur, ScalarValue::Bit(b'z'))
                || matches!(cur, ScalarValue::Vector(v) if v.iter().any(|b| *b == b'z'));
            let was = prev
                .map(|p| {
                    matches!(p, ScalarValue::Bit(b'z'))
                        || matches!(p, ScalarValue::Vector(v) if v.iter().any(|b| *b == b'z'))
                })
                .unwrap_or(false);
            now && !was
        }
    }
}

/// 把一个条件表达式解析成 (信号名, 条件)
fn cond_of(
    expr: &Value,
    env: &Environment,
    who: &str,
) -> Result<(String, FindCondition), String> {
    let resolved = crate::wal::builtins::signal::resolve_cond_names(expr, env);
    if let Some((n, c)) = crate::wal::builtins::signal::parse_edge_condition(&resolved) {
        return Ok((n, c));
    }
    if let Some((n, v)) = crate::wal::builtins::signal::parse_simple_condition(&resolved) {
        // `(= (get s) V)` 这类取值条件在"延迟/事件"语境里读作"变成 V 的那一刻"
        let c = if (0..=1).contains(&v) {
            FindCondition::Value(v as u8)
        } else {
            FindCondition::ValueI64(v)
        };
        return Ok((n, c));
    }
    Err(format!(
        "{}: 需要一个信号条件, 例如 (rising \"req\") / (falling \"ack\") / (changes \"v\") / (= (get \"v\") 3); 拿到: {}",
        who, resolved
    ))
}

/// 取某个条件在**时间域**上的命中时刻(不建索引空间 → 大波形上不触发全文件扫描)
fn cond_times(
    name: &str,
    cond: &FindCondition,
    who: &str,
    env: &Environment,
) -> Result<Vec<u64>, String> {
    let traces = env
        .get_traces()
        .ok_or_else(|| format!("{}: 没有加载波形(-l <file>)", who))?;
    let t = traces.read().unwrap_or_else(|e| e.into_inner());
    let tid = t
        .source_ids(&[name.to_string()])
        .into_iter()
        .next()
        .ok_or_else(|| format!("{}: 没有波形能解析出信号 '{}'", who, name))?;
    let tr = t.get(&tid).ok_or_else(|| format!("{}: trace 取不到", who))?;
    let resolved = tr
        .resolve_name(name)
        .ok_or_else(|| format!("{}: 解析不出信号名 '{}'", who, name))?;
    let init = tr.initial_value(&resolved);
    let points = tr.change_points_time(&resolved)?;
    let mut prev = init;
    let mut out: Vec<u64> = Vec::new();
    for (time, v) in &points {
        if edge_matches(prev.as_ref(), v, cond) {
            out.push(*time);
        }
        prev = Some(v.clone());
    }
    Ok(out)
}

/// 从选项表达式里认出符号名(`#restart` 会被解析成 `(resolve-group 'restart)`, 要递归找)
fn option_symbol(v: &Value) -> Option<String> {
    match v {
        // 跳过语法脚手架(resolve-group / quote), 只认载荷符号
        Value::Symbol(s) => {
            let n = s.name.trim_start_matches('#');
            if n == "resolve-group" || n == "quote" {
                None
            } else {
                Some(n.to_string())
            }
        }
        Value::List(l) => l.0.iter().find_map(option_symbol),
        _ => None,
    }
}

/// `(latency <start> <end> [#restart|#fifo])` → 每次 start→end 的间隔(原生时间单位)
///
/// *= 握手/请求→响应延迟分布*。默认 FIFO 配对;`#restart` 让新 start 作废未配对的旧
/// start(重试/覆盖语义)。配不上的两端直接丢弃 —— 不编造负延迟。
fn op_latency(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    if args.len() < 2 {
        return Err("(latency <start-edge> <end-edge> [#restart]) expected".to_string());
    }
    let mut restart = false;
    for a in &args[2..] {
        // ⚠️ `#restart` 在 AST 里**不是符号**而是 `(resolve-group 'restart)`(见 AGENTS 的
        // 分组符号规则), 所以这里递归找第一个符号名, 不能只 match Value::Symbol。
        let name = option_symbol(a)
            .ok_or_else(|| format!("latency: 选项必须是 #restart / #fifo, 拿到 {}", a))?;
        match name.as_str() {
            "restart" => restart = true,
            "fifo" => restart = false,
            other => return Err(format!("latency: 未知选项 #{} (可选 #fifo / #restart)", other)),
        }
    }
    let (sname, scond) = cond_of(&args[0], env, "latency")?;
    let (ename, econd) = cond_of(&args[1], env, "latency")?;
    let starts = cond_times(&sname, &scond, "latency(start)", env)?;
    let ends = cond_times(&ename, &econd, "latency(end)", env)?;

    let mut queue: std::collections::VecDeque<u64> = std::collections::VecDeque::new();
    let mut out: Vec<Value> = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < starts.len() || j < ends.len() {
        let take_start = match (starts.get(i), ends.get(j)) {
            (Some(s), Some(e)) => s <= e,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };
        if take_start {
            if restart {
                queue.clear();
            }
            queue.push_back(starts[i]);
            i += 1;
        } else {
            let e = ends[j];
            j += 1;
            if let Some(s) = queue.pop_front() {
                if e >= s {
                    out.push(Value::Int((e - s) as i64));
                }
            }
        }
    }
    if std::env::var("WAL_DEBUG_METRICS").is_ok() {
        eprintln!(
            "[latency] {}→{}: {} start, {} end, 配成 {} 对(未配对 {}){}",
            sname,
            ename,
            starts.len(),
            ends.len(),
            out.len(),
            starts.len().saturating_sub(out.len()),
            if restart { ", #restart" } else { ", #fifo" }
        );
    }
    Ok(Value::List(WList::from_vec(out)))
}

/// `(ipc <events> <cycles>)` → 事件数 / 周期数(**时间域**统计, 不建索引空间)
///
/// 典型用法: `(ipc (rising "inst_retire") (rising "clk"))`。
/// 想按窗口算就自己拿 `count` 组合。
fn op_ipc(args: &[Value], env: &mut Environment, _eval: &mut Evaluator) -> Result<Value, String> {
    if args.len() != 2 {
        return Err("(ipc <events-edge> <cycles-edge>) expected".to_string());
    }
    let (ename, econd) = cond_of(&args[0], env, "ipc")?;
    let (cname, ccond) = cond_of(&args[1], env, "ipc")?;
    let traces = env
        .get_traces()
        .ok_or_else(|| "ipc: 没有加载波形(-l <file>)".to_string())?;
    let (events, cycles) = {
        let t = traces.read().unwrap_or_else(|e| e.into_inner());
        let pick = |name: &str| -> Result<(&dyn Trace, String), String> {
            let tid = t
                .source_ids(&[name.to_string()])
                .into_iter()
                .next()
                .ok_or_else(|| format!("ipc: 没有波形能解析出 '{}'", name))?;
            let tr = t.get(&tid).ok_or_else(|| "ipc: trace 取不到".to_string())?;
            let resolved = tr
                .resolve_name(name)
                .ok_or_else(|| format!("ipc: 解析不出信号名 '{}'", name))?;
            Ok((tr, resolved))
        };
        let (et, er) = pick(&ename)?;
        let (ct, cr) = pick(&cname)?;
        (
            et.count_matches(&er, econd.clone())?,
            ct.count_matches(&cr, ccond.clone())?,
        )
    };
    if cycles == 0 {
        return Err(format!(
            "ipc: 周期信号 '{}' 在这个波形上没有边沿(分母为 0)",
            cname
        ));
    }
    Ok(Value::Float(events as f64 / cycles as f64))
}

pub fn register_metrics(disp: &mut Dispatcher) {
    disp.register(Operator::Median, op_median);
    disp.register(Operator::Percentile, op_percentile);
    disp.register(Operator::Stddev, op_stddev);
    disp.register(Operator::Stats, op_stats);
    disp.register(Operator::Histogram, op_histogram);
    disp.register(Operator::Latency, op_latency);
    disp.register(Operator::Ipc, op_ipc);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_matches_numpy_default() {
        let xs = vec![1.0, 2.0, 3.0, 4.0];
        assert_eq!(percentile(&xs, 50.0), 2.5);
        assert_eq!(percentile(&xs, 0.0), 1.0);
        assert_eq!(percentile(&xs, 100.0), 4.0);
        assert_eq!(percentile(&xs, 25.0), 1.75);
        assert!(percentile(&[], 50.0).is_nan());
    }

    #[test]
    fn stddev_is_sample_stddev() {
        assert_eq!(sample_stddev(&[]), 0.0);
        assert_eq!(sample_stddev(&[7.0]), 0.0);
        // [2,4,4,4,5,5,7,9]: 样本标准差 = 2.138...
        let xs = vec![2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
        assert!((sample_stddev(&xs) - 2.13809).abs() < 1e-4);
    }

    #[test]
    fn edge_matches_follows_query_engine_semantics() {
        let zero = ScalarValue::Bit(b'0');
        let one = ScalarValue::Bit(b'1');
        let x = ScalarValue::Bit(b'x');
        // x → 1 是 Changed, 不是 Rising(与查询引擎 §1 一致)
        assert!(!edge_matches(Some(&x), &one, &FindCondition::Rising));
        assert!(edge_matches(Some(&x), &one, &FindCondition::Changed));
        assert!(edge_matches(Some(&zero), &one, &FindCondition::Rising));
        assert!(!edge_matches(None, &one, &FindCondition::Rising));
        assert!(edge_matches(None, &one, &FindCondition::Changed) == false);
    }

    #[test]
    fn histogram_bins_cover_max_value() {
        let v = histogram_value(&[0.0, 1.0, 2.0, 3.0], 2);
        match v {
            Value::List(l) => {
                assert_eq!(l.0.len(), 2 * 3);
                let last = l.0.last().unwrap();
                assert_eq!(format!("{}", last), "2"); // 最大值落在最后一桶
            }
            other => panic!("expected list, got {:?}", other),
        }
    }
}
