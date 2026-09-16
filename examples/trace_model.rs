//! 波形后端"模型打印"工具: 把某个 Trace 后端的内部模型原样打出来,
//! 用于 VCD/FST/FSDB 三个后端的**逐索引差分对比**(排查语义漂移).
//!
//! 用法:
//!   cargo run --release --example trace_model -- file.vcd [sig...]
//!
//! 输出: max_index / timescale / 每个信号的 initial / change points /
//!       以及索引 0..=max_index 的取值表。

use wal_rust::trace::{Trace, TraceContainer};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: trace_model <waveform> [sig...]");
        std::process::exit(2);
    }
    let path = std::path::PathBuf::from(&args[1]);
    let mut c = TraceContainer::new();
    if let Err(e) = c.load(&path, "t".to_string()) {
        eprintln!("load error: {}", e);
        std::process::exit(1);
    }
    let tid = "t".to_string();
    let tr = c.get(&tid).expect("trace");
    let max = tr.max_index();
    println!("file      = {}", tr.filename());
    println!("max_index = {}", max);
    println!("timescale = {:?}", tr.timescale_exp());
    println!("signals   = {}", tr.signals().len());
    println!("scopes    = {}", tr.scopes().len());

    let names: Vec<String> = if args.len() > 2 {
        args[2..].to_vec()
    } else {
        tr.signals()
    };
    for n in &names {
        let full = tr.resolve_name(n).unwrap_or_else(|| n.clone());
        let w = tr.signal_width(&full).unwrap_or(0);
        println!("\nsig       = {}", full);
        println!("  width     = {}", w);
        println!("  initial   = {:?}", tr.initial_value(&full));
        println!("  defined   = {:?}", tr.defined_initial_value(&full));
        let cp = tr.change_points(&full).unwrap_or_default();
        println!("  changes   = {} 个: {:?}", cp.len(),
                 cp.iter().take(12).collect::<Vec<_>>());
        let mut row = Vec::new();
        for i in 0..=max {
            let sv = tr.signal_value(&full, i).unwrap_or_else(|e| wal_rust::trace::ScalarValue::Real(
                if e.is_empty() { 0.0 } else { f64::NAN }));
            row.push(format!("{}:{:?}", i, sv));
        }
        println!("  per-index = {}", row.join(" "));
    }
    // 时间线(索引 → 原生时间)
    let tl: Vec<String> = (0..=max.min(64)).map(|i| {
        format!("{}:{}", i, tr.timestamp_at(i).map(|t| t.to_string()).unwrap_or("?".into()))
    }).collect();
    println!("\ntimeline  = {}", tl.join(" "));
}
