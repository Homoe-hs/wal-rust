//! Goal-2 指标工具的第一批闸: 统计(median/percentile/stddev/stats/histogram) +
//! 波形指标(latency/ipc)。夹具是**写在测试里的小 VCD**(时间点固定), 所以期望值是手算的。

mod common;

use std::io::Write;
use wal_rust::wal::ast::Value;
use wal_rust::wal::eval::Evaluator;

/// 写一份夹具 VCD 到临时目录, 返回路径。
///
/// 时间线(1ns 时基, 全部手算过):
///
/// | 时刻 | 事件 |
/// |---|---|
/// | 0 | dumpvars: req=0 ack=0 clk=0 |
/// | 10 | req↑, clk↑ |
/// | 20 | ack↑, clk↑  → 延迟 10 |
/// | 25 | req↓ |
/// | 30 | ack↓, clk↑ |
/// | 50 | req↑ |
/// | 54 | ack↑  → 延迟 4 |
/// | 60 | req↓, clk↑ |
/// | 62 | ack↓ |
/// | 100 | req↑(第三次) |
/// | 110 | ack↑  → 延迟 10, FIFO 配到 100 那次 |
fn handshake_vcd() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("wal-metrics-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("handshake.vcd");
    let mut body = String::from(
        "$timescale 1ns $end\n$scope module tb $end\n\
         $var wire 1 ! req $end\n$var wire 1 \" ack $end\n$var wire 1 # clk $end\n$var wire 1 $ idle $end\n\
         $enddefinitions $end\n#0\n$dumpvars\n0!\n0\"\n0#\n0$\n$end\n",
    );
    for (t, req, ack, clk) in [
        (10, Some(true), None, Some(true)),
        (20, None, Some(true), Some(false)),
        (25, Some(false), None, None),
        (30, None, Some(false), Some(true)),
        (50, Some(true), None, None),
        (54, None, Some(true), None),
        (60, Some(false), None, Some(false)),
        (62, None, Some(false), None),
        (100, Some(true), None, None),
        (110, None, Some(true), None),
    ] {
        body.push_str(&format!("#{}\n", t));
        if let Some(v) = req {
            body.push_str(&format!("{}!\n", if v { 1 } else { 0 }));
        }
        if let Some(v) = ack {
            body.push_str(&format!("{}\"\n", if v { 1 } else { 0 }));
        }
        if let Some(v) = clk {
            body.push_str(&format!("{}#\n", if v { 1 } else { 0 }));
        }
    }
    let mut f = std::fs::File::create(&p).unwrap();
    f.write_all(body.as_bytes()).unwrap();
    p
}

/// 另一份夹具: 用来量 `#restart`(新 start 作废未配对的旧 start)
/// req↑@10, ack↑@30, req↑@20 → FIFO 会配 [10→30, 20→?]; restart 只留最后一次 start。
fn restart_vcd() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("wal-metrics-r-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("restart.vcd");
    // 注意: 同一时刻先 0 再 1 会被"每时间戳最后一次写入"折叠掉(引擎语义), 所以
    // 下降与再次上升必须落在**不同**时间戳, 否则根本没有第二个上升沿。
    let body = "$timescale 1ns $end\n$scope module tb $end\n\
                $var wire 1 ! req $end\n$var wire 1 \" ack $end\n\
                $enddefinitions $end\n#0\n$dumpvars\n0!\n0\"\n$end\n\
                #10\n1!\n#15\n0!\n#20\n1!\n#25\n1\"\n#30\n0\"\n";
    std::fs::write(&p, body).unwrap();
    p
}

fn eval_str(eval: &mut Evaluator, src: &str) -> String {
    match eval.eval(src) {
        Ok(Value::List(l)) => {
            let parts: Vec<String> = l.0.iter().map(|v| format!("{}", v)).collect();
            format!("({})", parts.join(" "))
        }
        Ok(v) => format!("{}", v),
        Err(e) => format!("ERR {}", e),
    }
}

#[test]
fn stats_ops_on_literals() {
    let mut eval = Evaluator::new();
    assert_eq!(eval_str(&mut eval, "(median (list 1 2 3 4))"), "2.5");
    assert_eq!(eval_str(&mut eval, "(percentile (list 1 2 3 4) 90)"), "3.7");
    assert_eq!(eval_str(&mut eval, "(percentile (list 1 2 3 4) 100)"), "4");
    // 样本标准差(n-1): [2,4,4,4,5,5,7,9] → 2.13809
    let sd = eval_str(&mut eval, "(stddev (list 2 4 4 4 5 5 7 9))");
    assert!(sd.starts_with("2.138"), "stddev got {}", sd);
    // 空列表不报错
    assert_eq!(eval_str(&mut eval, "(median (list))"), "NaN");
    // percentile 越界要报错(不许静默 clamp 后当正常结果)
    assert!(eval_str(&mut eval, "(percentile (list 1 2) 101)").starts_with("ERR"));
    // 非数值要报错, 不静默跳过
    assert!(eval_str(&mut eval, "(median (list 1 \"x\"))").starts_with("ERR"));
}

#[test]
fn stats_returns_count_min_p50_mean_p90_p99_max_stddev() {
    let mut eval = Evaluator::new();
    // 返回 (count min p50 mean p90 p99 max stddev); 浮点按 f64 原样(打印那一行才做格式化)
    let r = eval_str(&mut eval, "(stats (list 1 2 3 4))");
    assert!(r.starts_with("(4 1 2.5 2.5 3.7 3.9"), "stats got {}", r);
    assert!(r.ends_with("4 1.2909944487358056)"), "stats got {}", r);
}

#[test]
fn histogram_covers_all_values() {
    let mut eval = Evaluator::new();
    let r = eval_str(&mut eval, "(histogram (list 0 1 2 3) 2)");
    // (lo0 hi0 count0 lo1 hi1 count1); 边界按 ×1000 取整
    assert_eq!(r, "(0 1500 2 1500 3000 2)");
}

#[test]
fn latency_pairs_fifo_by_default() {
    let f = handshake_vcd();
    let mut eval = Evaluator::new();
    eval.load_trace(&f.to_string_lossy(), "t0").unwrap();
    // 10→20 = 10; 50→54 = 4; 100→110 = 10
    assert_eq!(
        eval_str(&mut eval, "(latency (rising \"tb.req\") (rising \"tb.ack\"))"),
        "(10 4 10)"
    );
    // 直接喂统计: 中位数 10, 最大 10
    assert_eq!(
        eval_str(&mut eval, "(median (latency (rising \"tb.req\") (rising \"tb.ack\")))"),
        "10"
    );
    let r = eval_str(
        &mut eval,
        "(stats (latency (rising \"tb.req\") (rising \"tb.ack\")))",
    );
    assert!(r.starts_with("(3 4 10 8 10 10 10"), "stats got {}", r);
}

#[test]
fn latency_restart_drops_superseded_start() {
    let f = restart_vcd();
    let mut eval = Evaluator::new();
    eval.load_trace(&f.to_string_lossy(), "t0").unwrap();
    // FIFO: start@10 配 ack@25 → 15; start@20 没 end 了 → 丢弃
    assert_eq!(
        eval_str(&mut eval, "(latency (rising \"tb.req\") (rising \"tb.ack\"))"),
        "(15)"
    );
    // restart: 最后一次 start@20 配 ack@25 → 5
    assert_eq!(
        eval_str(
            &mut eval,
            "(latency (rising \"tb.req\") (rising \"tb.ack\") #restart)"
        ),
        "(5)"
    );
}

#[test]
fn ipc_counts_events_over_cycles() {
    let f = handshake_vcd();
    let mut eval = Evaluator::new();
    eval.load_trace(&f.to_string_lossy(), "t0").unwrap();
    // req 上升 3 次(10/50/100); clk 上升 2 次(10/30) → 1.5
    assert_eq!(
        eval_str(&mut eval, "(ipc (rising \"tb.req\") (rising \"tb.clk\"))"),
        "1.5"
    );
    // 分母为 0(常 0 的信号没有上升沿)要报错, 不许给 inf/NaN 当答案
    assert!(eval_str(&mut eval, "(ipc (rising \"tb.req\") (rising \"tb.idle\"))")
        .starts_with("ERR"));
}
